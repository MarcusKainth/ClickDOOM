//! Byte-for-byte comparison of `render`'s generated SQL text against the
//! Python original's output for the same inputs. A fixture is the Python
//! function's own output, captured verbatim; a bare `assert_eq!` against it
//! catches a whitespace or formatting drift as loudly as a wrong keyword.

use clickdoom_driver::checkpoint::{fb_hash, hex64};
use clickdoom_driver::render::{
    ansi_render_sql, dense_words_sql, frame_readout_fb_hash_sql, frame_readout_sql, ppm_render_sql,
    region_bytes_sql,
};

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("fixtures/render/", $name, ".sql"))
    };
}

#[test]
fn region_bytes_matches_python_output() {
    assert_eq!(region_bytes_sql("foo"), fixture!("region_bytes"));
}

#[test]
fn dense_words_matches_python_output() {
    assert_eq!(
        dense_words_sql("db1", "framebuffer", 16000),
        fixture!("dense_words")
    );
}

#[test]
fn frame_readout_matches_python_output() {
    assert_eq!(frame_readout_sql("db1"), fixture!("frame_readout"));
}

#[test]
fn frame_readout_fb_hash_matches_python_output() {
    assert_eq!(
        frame_readout_fb_hash_sql("db1"),
        fixture!("frame_readout_fb_hash")
    );
}

#[test]
fn ansi_render_matches_python_output() {
    assert_eq!(ansi_render_sql("db1", 4, 2), fixture!("ansi_render"));
}

#[test]
fn ppm_render_matches_python_output() {
    assert_eq!(ppm_render_sql("db1", 4, 2), fixture!("ppm_render"));
}

#[test]
fn frame_readout_fb_hash_sql_reuses_checkpoints_fb_hash_and_hex64() {
    let expected = format!("SELECT {} AS fbhash\n", hex64(&fb_hash("fb", "palette")));
    assert!(frame_readout_fb_hash_sql("db1").starts_with(&expected));
}
