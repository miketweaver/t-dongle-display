// T-Dongle-S3 serial display
//
// Talks over the ESP32-S3's built-in USB-Serial/JTAG peripheral (VID:PID 303a:1001).
// Because that peripheral is hardware (not TinyUSB), esptool can always reset the
// chip into the ROM bootloader over USB — no BOOT button needed, even if this
// firmware hangs.
//
// Protocol: newline-terminated text lines. See README.md for the command list.
// Any line that is not a recognized command is shown as a big wrapped message.

#include <LovyanGFX.hpp>
#include <Preferences.h>

#define FW_NAME    "tdongle-display"
#define FW_VERSION "1.0.0"

// ---- T-Dongle-S3 pins ----
static constexpr int PIN_LCD_MOSI = 3;
static constexpr int PIN_LCD_SCLK = 5;
static constexpr int PIN_LCD_CS   = 4;
static constexpr int PIN_LCD_DC   = 2;
static constexpr int PIN_LCD_RST  = 1;
static constexpr int PIN_LCD_BL   = 38;  // active low
static constexpr int PIN_LED_DI   = 40;  // APA102 data
static constexpr int PIN_LED_CI   = 39;  // APA102 clock

class LGFX : public lgfx::LGFX_Device {
  lgfx::Panel_ST7735S _panel;
  lgfx::Bus_SPI _bus;
  lgfx::Light_PWM _light;

 public:
  LGFX() {
    {
      auto cfg = _bus.config();
      cfg.spi_host = SPI2_HOST;
      cfg.spi_mode = 0;
      cfg.freq_write = 27000000;
      cfg.freq_read = 16000000;
      cfg.pin_sclk = PIN_LCD_SCLK;
      cfg.pin_mosi = PIN_LCD_MOSI;
      cfg.pin_miso = -1;
      cfg.pin_dc = PIN_LCD_DC;
      cfg.dma_channel = SPI_DMA_CH_AUTO;
      _bus.config(cfg);
      _panel.setBus(&_bus);
    }
    {
      auto cfg = _panel.config();
      cfg.pin_cs = PIN_LCD_CS;
      cfg.pin_rst = PIN_LCD_RST;
      cfg.pin_busy = -1;
      cfg.memory_width = 132;
      cfg.memory_height = 162;
      cfg.panel_width = 80;
      cfg.panel_height = 160;
      cfg.offset_x = 26;
      cfg.offset_y = 1;
      cfg.offset_rotation = 0;
      cfg.readable = false;
      cfg.invert = true;
      cfg.rgb_order = false;
      cfg.dlen_16bit = false;
      cfg.bus_shared = false;
      _panel.config(cfg);
    }
    {
      auto cfg = _light.config();
      cfg.pin_bl = PIN_LCD_BL;
      cfg.invert = true;
      cfg.freq = 12000;
      cfg.pwm_channel = 7;
      _light.config(cfg);
      _panel.setLight(&_light);
    }
    setPanel(&_panel);
  }
};

static LGFX lcd;
static String line;
static int cursorY = 0;  // for LOG mode

// ---- power ----
// Screen sleep: after `sleepSecs` with no commands, backlight off + panel SLPIN.
// Any command wakes it. 0 = never. Persisted in NVS.
static Preferences prefs;
static uint8_t brightness = 160;
static uint32_t sleepSecs = 300;
static uint32_t lastActivity = 0;
static bool screenAsleep = false;

static void screenWake() {
  if (screenAsleep) { lcd.wakeup(); lcd.setBrightness(brightness); screenAsleep = false; }
  lastActivity = millis();
}
static void screenSleep() {
  if (!screenAsleep) { lcd.sleep(); screenAsleep = true; }
}

// ---------------- APA102 LED (bit-banged, single pixel) ----------------
static void ledByte(uint8_t b) {
  for (int i = 7; i >= 0; --i) {
    digitalWrite(PIN_LED_DI, (b >> i) & 1);
    digitalWrite(PIN_LED_CI, HIGH);
    digitalWrite(PIN_LED_CI, LOW);
  }
}
static void ledSet(uint8_t r, uint8_t g, uint8_t b, uint8_t bright = 8) {
  for (int i = 0; i < 4; ++i) ledByte(0x00);
  ledByte(0xE0 | (bright & 0x1F));
  ledByte(b); ledByte(g); ledByte(r);
  for (int i = 0; i < 4; ++i) ledByte(0xFF);
}

// ---------------- parsing helpers ----------------
struct NamedColor { const char* name; uint32_t rgb; };
static const NamedColor kColors[] = {
  {"black", 0x000000}, {"white", 0xFFFFFF}, {"red", 0xFF0000}, {"green", 0x00FF00},
  {"blue", 0x0000FF},  {"yellow", 0xFFFF00}, {"cyan", 0x00FFFF}, {"magenta", 0xFF00FF},
  {"orange", 0xFF8000}, {"purple", 0x8000FF}, {"gray", 0x808080}, {"grey", 0x808080},
  {"pink", 0xFF60A0}, {"navy", 0x000080}, {"darkgreen", 0x006400}, {"brown", 0x8B4513},
};

static bool parseColor(const String& s, uint32_t& out) {
  String t = s; t.trim(); t.toLowerCase();
  for (auto& c : kColors) if (t == c.name) { out = c.rgb; return true; }
  if (t.startsWith("#")) t.remove(0, 1);
  if (t.startsWith("0x")) t.remove(0, 2);
  if (t.length() != 6) return false;
  char* end;
  out = strtoul(t.c_str(), &end, 16);
  return *end == 0;
}
static uint16_t c565(uint32_t rgb) { return lcd.color565(rgb >> 16, (rgb >> 8) & 0xFF, rgb & 0xFF); }

// Splits `s` into up to `max` whitespace-separated tokens; the last token gets the remainder.
static int tokenize(const String& s, String* out, int max) {
  int n = 0, i = 0, len = s.length();
  while (n < max && i < len) {
    while (i < len && s[i] == ' ') ++i;
    if (i >= len) break;
    if (n == max - 1) { out[n++] = s.substring(i); break; }
    int j = i;
    while (j < len && s[j] != ' ') ++j;
    out[n++] = s.substring(i, j);
    i = j;
  }
  return n;
}

static bool toInt(const String& s, int& v) {
  char* end; long x = strtol(s.c_str(), &end, 10);
  if (end == s.c_str() || *end) return false;
  v = (int)x; return true;
}

// ---------------- drawing ----------------
static void showMessage(const String& msg, uint32_t fg = 0xFFFFFF, uint32_t bg = 0x000000) {
  lcd.fillScreen(c565(bg));
  lcd.setTextColor(c565(fg), c565(bg));
  lcd.setTextWrap(false);
  // Pick the largest font scale where the word-wrapped text fits.
  for (int sz = 4; sz >= 1; --sz) {
    lcd.setFont(&fonts::Font0);
    lcd.setTextSize(sz);
    int cw = 6 * sz, ch = 8 * sz;
    int cols = lcd.width() / cw;
    int rows = lcd.height() / ch;
    if (cols < 1) continue;
    // greedy word wrap
    std::vector<String> out;
    String cur;
    int i = 0;
    while (i <= (int)msg.length()) {
      int j = msg.indexOf(' ', i);
      if (j < 0) j = msg.length();
      String w = msg.substring(i, j);
      if ((int)w.length() > cols && sz > 1) { out.assign(rows + 1, String()); break; }  // don't split words; try smaller
      while ((int)w.length() > cols) {  // hard-split long words (only at size 1)
        if (cur.length()) { out.push_back(cur); cur = ""; }
        out.push_back(w.substring(0, cols));
        w = w.substring(cols);
      }
      if (!cur.length()) cur = w;
      else if ((int)(cur.length() + 1 + w.length()) <= cols) cur += " " + w;
      else { out.push_back(cur); cur = w; }
      i = j + 1;
    }
    if (cur.length()) out.push_back(cur);
    if ((int)out.size() > rows && sz > 1) continue;
    int y0 = (lcd.height() - (int)out.size() * ch) / 2;
    if (y0 < 0) y0 = 0;
    for (size_t k = 0; k < out.size(); ++k) {
      int x = (lcd.width() - (int)out[k].length() * cw) / 2;
      lcd.drawString(out[k], x, y0 + k * ch);
    }
    return;
  }
}

static void logLine(const String& s) {
  lcd.setFont(&fonts::Font0);
  lcd.setTextSize(1);
  lcd.setTextColor(TFT_WHITE, TFT_BLACK);
  int cols = lcd.width() / 6;
  for (int i = 0; i < (int)s.length() || i == 0; i += cols) {
    if (cursorY + 8 > lcd.height()) {
      lcd.scroll(0, -8);
      cursorY -= 8;
    }
    lcd.fillRect(0, cursorY, lcd.width(), 8, TFT_BLACK);
    lcd.drawString(s.substring(i, i + cols), 0, cursorY);
    cursorY += 8;
  }
}

static void splash() {
  lcd.fillScreen(TFT_BLACK);
  lcd.setFont(&fonts::Font0);
  lcd.setTextSize(1);
  lcd.setTextColor(TFT_CYAN, TFT_BLACK);
  lcd.drawCenterString("T-Dongle", lcd.width() / 2, 20);
  lcd.setTextColor(TFT_WHITE, TFT_BLACK);
  lcd.drawCenterString("serial", lcd.width() / 2, 40);
  lcd.drawCenterString("display", lcd.width() / 2, 50);
  lcd.setTextColor(TFT_DARKGREY, TFT_BLACK);
  lcd.drawCenterString("v" FW_VERSION, lcd.width() / 2, 70);
  lcd.fillRect(0, lcd.height() - 30, lcd.width() / 3, 10, TFT_RED);
  lcd.fillRect(lcd.width() / 3, lcd.height() - 30, lcd.width() / 3, 10, TFT_GREEN);
  lcd.fillRect(2 * lcd.width() / 3, lcd.height() - 30, lcd.width() - 2 * lcd.width() / 3, 10, TFT_BLUE);
  lcd.setTextColor(TFT_YELLOW, TFT_BLACK);
  lcd.drawCenterString("waiting", lcd.width() / 2, lcd.height() - 14);
  cursorY = 0;
}

// ---------------- command dispatch ----------------
static void reply(const char* s) { Serial.println(s); }
static void err(const String& s) { Serial.print("ERR "); Serial.println(s); }

static void handleLine(String l) {
  l.trim();
  if (!l.length()) return;

  String cmd = l;
  String rest;
  int sp = l.indexOf(' ');
  if (sp >= 0) { cmd = l.substring(0, sp); rest = l.substring(sp + 1); rest.trim(); }
  String CMD = cmd; CMD.toUpperCase();
  String a[8];
  uint32_t col;

  // Status queries don't count as activity (so a poller won't keep the screen on).
  bool query = CMD == "PING" || CMD == "INFO" || CMD == "ID" || CMD == "TEMP" || CMD == "HELP" || CMD == "SLEEP";
  if (!query) screenWake();

  if (CMD == "PING") { reply("PONG"); return; }
  if (CMD == "INFO" || CMD == "ID") {
    Serial.printf("OK %s %s %dx%d rot=%d bl=%u sleep=%lus asleep=%d\n", FW_NAME, FW_VERSION, lcd.width(), lcd.height(),
                  lcd.getRotation(), brightness, sleepSecs, screenAsleep);
    return;
  }
  if (CMD == "HELP") {
    reply("OK commands: PING INFO HELP MSG <text> | CLEAR [c] | TEXT x y size c <text> |"
          " LOG <text> | PIXEL x y c | LINE x0 y0 x1 y1 c | RECT x y w h c | FILL x y w h c |"
          " CIRCLE x y r c | FCIRCLE x y r c | ROT 0-3 | BL 0-255 | LED c [bright0-31] | SPLASH |"
          " SLEEP [secs] | WAKE | TEMP."
          " c = name or RRGGBB. Unrecognized lines are shown as MSG.");
    return;
  }
  if (CMD == "MSG") { showMessage(rest); reply("OK"); return; }
  if (CMD == "CMSG") {  // CMSG fg bg text
    uint32_t fg, bg;
    if (tokenize(rest, a, 3) < 3 || !parseColor(a[0], fg) || !parseColor(a[1], bg)) return err("usage: CMSG fg bg text");
    showMessage(a[2], fg, bg); reply("OK"); return;
  }
  if (CMD == "CLEAR") {
    col = 0;
    if (rest.length() && !parseColor(rest, col)) return err("bad color");
    lcd.fillScreen(c565(col)); cursorY = 0; reply("OK"); return;
  }
  if (CMD == "SPLASH") { splash(); reply("OK"); return; }
  if (CMD == "LOG") { logLine(rest); reply("OK"); return; }
  if (CMD == "TEXT") {
    int x, y, s;
    if (tokenize(rest, a, 5) < 5 || !toInt(a[0], x) || !toInt(a[1], y) || !toInt(a[2], s) || !parseColor(a[3], col))
      return err("usage: TEXT x y size color text");
    lcd.setFont(&fonts::Font0);
    lcd.setTextSize(constrain(s, 1, 8));
    lcd.setTextColor(c565(col));
    lcd.drawString(a[4], x, y);
    reply("OK"); return;
  }
  if (CMD == "PIXEL") {
    int x, y;
    if (tokenize(rest, a, 3) < 3 || !toInt(a[0], x) || !toInt(a[1], y) || !parseColor(a[2], col)) return err("usage: PIXEL x y color");
    lcd.drawPixel(x, y, c565(col)); reply("OK"); return;
  }
  if (CMD == "LINE" || CMD == "RECT" || CMD == "FILL") {
    int v[4];
    if (tokenize(rest, a, 5) < 5 || !parseColor(a[4], col)) return err("usage: " + CMD + " a b c d color");
    for (int i = 0; i < 4; ++i) if (!toInt(a[i], v[i])) return err("bad number");
    if (CMD == "LINE") lcd.drawLine(v[0], v[1], v[2], v[3], c565(col));
    else if (CMD == "RECT") lcd.drawRect(v[0], v[1], v[2], v[3], c565(col));
    else lcd.fillRect(v[0], v[1], v[2], v[3], c565(col));
    reply("OK"); return;
  }
  if (CMD == "CIRCLE" || CMD == "FCIRCLE") {
    int x, y, r;
    if (tokenize(rest, a, 4) < 4 || !toInt(a[0], x) || !toInt(a[1], y) || !toInt(a[2], r) || !parseColor(a[3], col))
      return err("usage: " + CMD + " x y r color");
    if (CMD == "CIRCLE") lcd.drawCircle(x, y, r, c565(col)); else lcd.fillCircle(x, y, r, c565(col));
    reply("OK"); return;
  }
  if (CMD == "ROT") {
    int r;
    if (!toInt(rest, r) || r < 0 || r > 3) return err("usage: ROT 0-3");
    lcd.setRotation(r); lcd.fillScreen(TFT_BLACK); cursorY = 0;
    Serial.printf("OK %dx%d\n", lcd.width(), lcd.height()); return;
  }
  if (CMD == "BL") {
    int b;
    if (!toInt(rest, b)) return err("usage: BL 0-255");
    brightness = constrain(b, 0, 255);
    lcd.setBrightness(brightness);
    prefs.putUChar("bl", brightness);
    reply("OK"); return;
  }
  if (CMD == "SLEEP") {  // SLEEP            -> sleep screen now
    if (!rest.length()) {  // SLEEP <seconds>  -> idle timeout (0 = never), saved
      screenSleep(); reply("OK"); return;
    }
    int v;
    if (!toInt(rest, v) || v < 0) return err("usage: SLEEP [seconds]");
    sleepSecs = v; prefs.putUInt("sleep", sleepSecs);
    Serial.printf("OK sleep=%lus\n", sleepSecs); return;
  }
  if (CMD == "WAKE") { reply("OK"); return; }  // screenWake() already ran
  if (CMD == "TEMP") { Serial.printf("OK %.1fC cpu=%luMHz\n", temperatureRead(), getCpuFrequencyMhz()); return; }
  if (CMD == "GPIO") {  // debug: GPIO pin 0|1
    int p, v;
    if (tokenize(rest, a, 2) < 2 || !toInt(a[0], p) || !toInt(a[1], v)) return err("usage: GPIO pin 0|1");
    pinMode(p, OUTPUT); digitalWrite(p, v); reply("OK"); return;
  }
  if (CMD == "LED") {
    int br = 8, n = tokenize(rest, a, 2);
    if (n < 1 || !parseColor(a[0], col) || (n == 2 && !toInt(a[1], br))) return err("usage: LED color [bright0-31]");
    ledSet(col >> 16, (col >> 8) & 0xFF, col & 0xFF, br); reply("OK"); return;
  }

  // Not a command: show the whole line as a message.
  showMessage(l);
  reply("OK");
}

void setup() {
  // 80 MHz is plenty for a text display and roughly halves CPU power vs 240.
  // (Automatic light sleep is NOT used: it gates the USB-Serial/JTAG clock, the
  // host would see the port drop, and button-free flashing would break.)
  setCpuFrequencyMhz(80);
  Serial.setTxTimeoutMs(0);  // never block if no host has the port open
  Serial.begin(115200);

  pinMode(PIN_LED_DI, OUTPUT);
  pinMode(PIN_LED_CI, OUTPUT);
  ledSet(0, 0, 0, 0);

  lcd.init();
  lcd.setRotation(0);
  prefs.begin("tdongle", false);
  brightness = prefs.getUChar("bl", brightness);
  sleepSecs = prefs.getUInt("sleep", sleepSecs);
  lcd.setBrightness(brightness);
  lastActivity = millis();
  splash();
  line.reserve(512);
}

void loop() {
  while (Serial.available()) {
    char c = Serial.read();
    if (c == '\n' || c == '\r') {
      if (line.length()) handleLine(line);
      line = "";
    } else if (line.length() < 1024) {
      line += c;
    }
  }
  if (sleepSecs && !screenAsleep && millis() - lastActivity > sleepSecs * 1000UL) screenSleep();
  delay(10);
}
