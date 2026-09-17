//! Basic IME example
//!
//! Demonstrates using imekit to register as an input method
//! and handle IME events.
//!
//! # Usage
//!
//! Run this on a system with Wayland or X11:
//! ```
//! cargo run --example basic_ime
//! ```

use imekit::{InputMethod, InputMethodEvent};

fn main() -> Result<(), imekit::Error> {
    env_logger::init();

    let mut im = InputMethod::new()?;

    #[cfg(target_os = "linux")]
    {
        if im.is_wayland() {
            println!("Using Wayland backend (zwp_input_method_v2)");
        } else if im.is_x11() {
            println!("Using X11 backend (XIM)");
        }
        #[cfg(feature = "ibus")]
        if im.is_ibus() {
            println!("Using IBus backend (D-Bus)");
        }
    }

    // Main event loop - process events until interrupted
    loop {
        // Process IME events
        while let Some(event) = im.next_event() {
            match event {
                InputMethodEvent::Activate { serial } => {
                    im.set_preedit_string("你好", 0, 6)?;
                    // println!("  - Ready to receive text input");
                }
                InputMethodEvent::Deactivate => {
                    // println!("IME DEACTIVATED");
                }
                InputMethodEvent::SurroundingText {
                    text,
                    cursor,
                    anchor,
                } => {
                    println!("SURROUNDING TEXT:");
                    println!("  Text: {:?}", text);
                    println!("  Cursor: {} bytes", cursor);
                    println!("  Anchor: {} bytes", anchor);
                }
                InputMethodEvent::ContentType { hint, purpose } => {
                    println!("CONTENT TYPE:");
                    println!("  Hint: {:?}", hint);
                    println!("  Purpose: {:?}", purpose);
                }
                InputMethodEvent::TextChangeCause(cause) => {
                    println!("TEXT CHANGE CAUSE: {:?}", cause);
                }
                InputMethodEvent::Done => {
                    println!("DONE (all pending state received)");

                    // Example: check IME state
                    if im.is_active() {
                        println!("  IME is active");
                    }
                }
                InputMethodEvent::PopupSurfaceCreated {
                    x,
                    y,
                    width,
                    height,
                } => {
                    println!("POPUP SURFACE CREATED:");
                    println!("  Position: ({}, {})", x, y);
                    println!("  Size: {}x{}", width, height);
                }
                InputMethodEvent::Unavailable => {
                    println!("IME UNAVAILABLE - compositor doesn't support the protocol");
                    return Ok(());
                }
                InputMethodEvent::KeyEvent {
                    keycode,
                    keysym,
                    state,
                    modifiers,
                } => {
                    println!(
                        "KeyEvent: keycode({}), keysym({}), state({:?}), modifiers({:?})",
                        keycode, keysym, state, modifiers
                    );
                }
                InputMethodEvent::RepeatInfo { rate, delay } => {
                    println!("KeyRepeat: rate({}) delay({})", rate, delay);
                }
            }
            println!();
        }

        // Small sleep to avoid busy-waiting
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
