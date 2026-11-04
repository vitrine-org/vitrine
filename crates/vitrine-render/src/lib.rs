//! Vitrine's widget renderer: a retained scene tree driven by widget scripts,
//! laid out with Taffy and painted with Direct2D / DirectWrite.

pub mod anim;
pub mod color;
pub mod layout;
pub mod path;
pub mod scene;
pub mod style;
pub mod value;

pub mod backdrop;
pub mod compose;
pub mod gpu;
pub mod image;
pub mod paint;
pub mod text;
pub mod widget;
