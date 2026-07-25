//! Deterministic semantic-view renderer for the fitted E290 e-paper panel.
//!
//! This module knows pixels but not GPIO, SPI, timing, or task orchestration.
//! Pairing digits are borrowed only inside the display model's exposure
//! callback and are never formatted into an allocating string.

use core::{convert::Infallible, str};

use embedded_graphics::{
    mono_font::{
        MonoTextStyle,
        ascii::{FONT_6X10, FONT_10X20},
    },
    pixelcolor::BinaryColor,
    prelude::*,
    primitives::{Line, PrimitiveStyle, Rectangle},
    text::{Alignment, Baseline, Text, TextStyleBuilder},
};
use reticulum_appliance_display_model::{DisplayView, DisplayViewKind};
use reticulum_eink_ssd1680::{E290_FRAME_HEIGHT, E290_FRAME_WIDTH, E290FrameBuffer};

const HORIZONTAL_CENTER: i32 = (E290_FRAME_WIDTH / 2) as i32;

/// Render one complete semantic view into a caller-owned full-frame buffer.
///
/// `Blank` is physically white. Every other view uses the visually qualified
/// black-background/white-foreground E290 polarity.
pub fn render_display_view(
    view: &DisplayView,
    frame: &mut E290FrameBuffer,
) -> Result<(), Infallible> {
    if view.kind() == DisplayViewKind::Blank {
        frame.clear(BinaryColor::On);
        return Ok(());
    }

    frame.clear(BinaryColor::Off);
    Rectangle::new(
        Point::zero(),
        Size::new(E290_FRAME_WIDTH as u32, E290_FRAME_HEIGHT as u32),
    )
    .into_styled(PrimitiveStyle::with_stroke(BinaryColor::On, 2))
    .draw(frame)?;
    Line::new(Point::new(10, 31), Point::new(285, 31))
        .into_styled(PrimitiveStyle::with_stroke(BinaryColor::On, 1))
        .draw(frame)?;

    if let Some(label) = view.label() {
        draw_centered(
            label.as_str(),
            10,
            MonoTextStyle::new(&FONT_6X10, BinaryColor::On),
            frame,
        )?;
    }

    match view {
        DisplayView::Blank => unreachable!("blank returned before framed rendering"),
        DisplayView::Booting { .. } => {
            draw_large_status("STARTING", frame)?;
            draw_footer("LORA MESH INITIALIZING", frame)?;
        }
        DisplayView::Ready { .. } => {
            draw_large_status("READY", frame)?;
            draw_footer("OPEN APP TO CONNECT", frame)?;
        }
        DisplayView::Pairing {
            expires_after_seconds,
            ..
        } => {
            draw_centered(
                "PAIRING CODE",
                39,
                MonoTextStyle::new(&FONT_6X10, BinaryColor::On),
                frame,
            )?;
            view.expose_pairing_passkey(|digits| {
                let digits = digits.expect("pairing views always own six validated digits");
                let digits =
                    str::from_utf8(digits).expect("PairingPasskey construction validates ASCII");
                draw_centered(
                    digits,
                    57,
                    MonoTextStyle::new(&FONT_10X20, BinaryColor::On),
                    frame,
                )
                .expect("the fixed framebuffer draw target is infallible");
            });
            let seconds = DecimalU16::new(expires_after_seconds.get());
            draw_centered(
                seconds.as_str(),
                88,
                MonoTextStyle::new(&FONT_6X10, BinaryColor::On),
                frame,
            )?;
            draw_centered(
                "SECONDS - CONFIRM IN APP",
                104,
                MonoTextStyle::new(&FONT_6X10, BinaryColor::On),
                frame,
            )?;
        }
        DisplayView::PairingSucceeded { .. } => {
            draw_large_status("PAIRED", frame)?;
            draw_footer("SECURE CONNECTION READY", frame)?;
        }
        DisplayView::PairingFailed { .. } => {
            draw_large_status("PAIR FAILED", frame)?;
            draw_footer("PRESS 21 TO TRY AGAIN", frame)?;
        }
        DisplayView::PairingTimedOut { .. } => {
            draw_large_status("TIMED OUT", frame)?;
            draw_footer("PRESS 21 TO TRY AGAIN", frame)?;
        }
    }

    Ok(())
}

fn draw_large_status(status: &str, frame: &mut E290FrameBuffer) -> Result<(), Infallible> {
    draw_centered(
        status,
        53,
        MonoTextStyle::new(&FONT_10X20, BinaryColor::On),
        frame,
    )
}

fn draw_footer(footer: &str, frame: &mut E290FrameBuffer) -> Result<(), Infallible> {
    draw_centered(
        footer,
        101,
        MonoTextStyle::new(&FONT_6X10, BinaryColor::On),
        frame,
    )
}

fn draw_centered(
    text: &str,
    y: i32,
    style: MonoTextStyle<'_, BinaryColor>,
    frame: &mut E290FrameBuffer,
) -> Result<(), Infallible> {
    let centered = TextStyleBuilder::new()
        .alignment(Alignment::Center)
        .baseline(Baseline::Top)
        .build();
    Text::with_text_style(text, Point::new(HORIZONTAL_CENTER, y), style, centered)
        .draw(frame)
        .map(|_| ())
}

struct DecimalU16 {
    bytes: [u8; 5],
    start: usize,
}

impl DecimalU16 {
    fn new(mut value: u16) -> Self {
        let mut bytes = [b'0'; 5];
        let mut start = bytes.len();
        loop {
            start -= 1;
            bytes[start] = b'0' + (value % 10) as u8;
            value /= 10;
            if value == 0 {
                break;
            }
        }
        Self { bytes, start }
    }

    fn as_str(&self) -> &str {
        str::from_utf8(&self.bytes[self.start..])
            .expect("the decimal formatter emits only ASCII digits")
    }
}

#[cfg(test)]
mod tests {
    use reticulum_appliance_display_model::{
        DisplayCommand, DisplayLabel, DisplayState, PairingPasskey, PairingSecretClearReason,
        PairingWindowSeconds,
    };

    use super::*;

    fn label() -> DisplayLabel {
        DisplayLabel::new("Reticulum E290").expect("fixture label fits")
    }

    fn render(command: DisplayCommand) -> E290FrameBuffer {
        let mut state = DisplayState::new();
        let _ = state.apply(command);
        let mut frame = E290FrameBuffer::new_white();
        render_display_view(state.view(), &mut frame).expect("render is infallible");
        frame
    }

    #[test]
    fn blank_is_the_physical_white_frame() {
        let state = DisplayState::new();
        let mut frame = E290FrameBuffer::new_white();
        render_display_view(state.view(), &mut frame).expect("render is infallible");
        assert!(frame.as_bytes().iter().all(|byte| *byte == u8::MAX));
    }

    #[test]
    fn every_public_non_secret_state_renders_deterministically() {
        let commands = [
            DisplayCommand::ShowBooting { label: label() },
            DisplayCommand::ShowReady { label: label() },
            DisplayCommand::ClearPairingSecret {
                label: label(),
                reason: PairingSecretClearReason::Succeeded,
            },
            DisplayCommand::ClearPairingSecret {
                label: label(),
                reason: PairingSecretClearReason::Failed,
            },
            DisplayCommand::ClearPairingSecret {
                label: label(),
                reason: PairingSecretClearReason::TimedOut,
            },
        ];

        for command in commands {
            let frame = render(command);
            assert!(frame.as_bytes().iter().any(|byte| *byte != 0));
            assert!(frame.as_bytes().iter().any(|byte| *byte != u8::MAX));
        }
    }

    #[test]
    fn pairing_digits_and_window_change_the_rendered_frame() {
        let first = render(DisplayCommand::ShowPairing {
            label: label(),
            passkey: PairingPasskey::from_number(123_456).expect("valid passkey"),
            expires_after_seconds: PairingWindowSeconds::new(60).expect("nonzero window"),
        });
        let second = render(DisplayCommand::ShowPairing {
            label: label(),
            passkey: PairingPasskey::from_number(123_457).expect("valid passkey"),
            expires_after_seconds: PairingWindowSeconds::new(59).expect("nonzero window"),
        });

        assert_ne!(first.as_bytes(), second.as_bytes());
    }

    #[test]
    fn decimal_formatter_covers_full_window_domain_without_allocation() {
        for (value, expected) in [
            (0, "0"),
            (1, "1"),
            (9, "9"),
            (10, "10"),
            (999, "999"),
            (65_535, "65535"),
        ] {
            assert_eq!(DecimalU16::new(value).as_str(), expected);
        }
    }
}
