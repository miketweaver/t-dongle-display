//! tdongle: draw on a LilyGO T-Dongle-S3 running tdongle-display firmware.
//! Zero dependencies: std plus a few hand-declared libc functions. Linux only.

pub mod cli;
mod device;
mod image;
mod json;
mod model;
mod render;
mod skill;
mod spec;
mod sys;
