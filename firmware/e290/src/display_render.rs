//! Deterministic semantic-view renderer for the fitted E290 e-paper panel.
//!
//! This module knows pixels but not GPIO, SPI, timing, or task orchestration.

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
use reticulum_appliance_display_model::{
    DisplayCompositionState, DisplayHomeSnapshot, DisplaySetupState, DisplayView, DisplayViewKind,
};
use reticulum_eink_ssd1680::{E290_FRAME_HEIGHT, E290_FRAME_WIDTH, E290FrameBuffer};

const HORIZONTAL_CENTER: i32 = (E290_FRAME_WIDTH / 2) as i32;

/// Render one complete semantic view into a caller-owned full-frame buffer.
///
/// `Blank` is physically white. Every other view uses the E290's selected
/// black-background/white-foreground polarity.
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
        DisplayView::Home { snapshot } => draw_home(*snapshot, frame)?,
    }

    Ok(())
}

fn draw_home(snapshot: DisplayHomeSnapshot, frame: &mut E290FrameBuffer) -> Result<(), Infallible> {
    let named = snapshot.appliance_label().is_some();
    let suffix_y = match snapshot.appliance_label() {
        Some(name) => {
            draw_centered(
                name.as_str(),
                34,
                MonoTextStyle::new(&FONT_6X10, BinaryColor::On),
                frame,
            )?;
            46
        }
        None => 38,
    };
    draw_centered(
        snapshot.device_suffix().as_str(),
        suffix_y,
        MonoTextStyle::new(&FONT_10X20, BinaryColor::On),
        frame,
    )?;
    draw_uncollected_badge(snapshot.uncollected_messages(), frame)?;
    let link_y = if named { 70 } else { 67 };
    let service_y = if named { 86 } else { 83 };
    draw_centered(
        link_configuration(snapshot),
        link_y,
        MonoTextStyle::new(&FONT_6X10, BinaryColor::On),
        frame,
    )?;
    draw_centered(
        service_configuration(snapshot),
        service_y,
        MonoTextStyle::new(&FONT_6X10, BinaryColor::On),
        frame,
    )?;
    draw_footer(
        match snapshot.setup() {
            DisplaySetupState::EnrollmentRequired => "HOLD GPIO21 TO ENROLL",
            DisplaySetupState::Enrolled => "READY - OPEN APP",
            DisplaySetupState::Unavailable => "LOCAL API UNAVAILABLE",
        },
        frame,
    )
}

fn draw_uncollected_badge(
    uncollected_messages: u32,
    frame: &mut E290FrameBuffer,
) -> Result<(), Infallible> {
    let Some(text) = UncollectedBadgeText::new(uncollected_messages) else {
        return Ok(());
    };
    Rectangle::new(Point::new(225, 36), Size::new(64, 23))
        .into_styled(PrimitiveStyle::with_stroke(BinaryColor::On, 1))
        .draw(frame)?;
    draw_centered_at(
        text.as_str(),
        Point::new(257, 43),
        MonoTextStyle::new(&FONT_6X10, BinaryColor::On),
        frame,
    )
}

const fn link_configuration(snapshot: DisplayHomeSnapshot) -> &'static str {
    match (snapshot.lora(), snapshot.ble()) {
        (DisplayCompositionState::Configured, DisplayCompositionState::Configured) => {
            "LORA NA915  RNS BLE"
        }
        (DisplayCompositionState::Configured, DisplayCompositionState::Unavailable) => {
            "LORA NA915  RNS BLE --"
        }
        (DisplayCompositionState::Unavailable, DisplayCompositionState::Configured) => {
            "LORA --  RNS BLE"
        }
        (DisplayCompositionState::Unavailable, DisplayCompositionState::Unavailable) => {
            "LORA --  RNS BLE --"
        }
    }
}

const fn service_configuration(snapshot: DisplayHomeSnapshot) -> &'static str {
    match (snapshot.lxmf(), snapshot.nomad()) {
        (DisplayCompositionState::Configured, DisplayCompositionState::Configured) => {
            "SERVICES LXMF + NOMAD"
        }
        (DisplayCompositionState::Configured, DisplayCompositionState::Unavailable) => {
            "SERVICE LXMF"
        }
        (DisplayCompositionState::Unavailable, DisplayCompositionState::Configured) => {
            "SERVICE NOMAD"
        }
        (DisplayCompositionState::Unavailable, DisplayCompositionState::Unavailable) => {
            "SERVICES UNAVAILABLE"
        }
    }
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
    draw_centered_at(text, Point::new(HORIZONTAL_CENTER, y), style, frame)
}

fn draw_centered_at(
    text: &str,
    position: Point,
    style: MonoTextStyle<'_, BinaryColor>,
    frame: &mut E290FrameBuffer,
) -> Result<(), Infallible> {
    let centered = TextStyleBuilder::new()
        .alignment(Alignment::Center)
        .baseline(Baseline::Top)
        .build();
    Text::with_text_style(text, position, style, centered)
        .draw(frame)
        .map(|_| ())
}

struct UncollectedBadgeText {
    bytes: [u8; 7],
    length: usize,
}

impl UncollectedBadgeText {
    fn new(count: u32) -> Option<Self> {
        if count == 0 {
            return None;
        }

        let mut bytes = *b"NEW 99+";
        let length = if count >= 100 {
            bytes.len()
        } else if count >= 10 {
            bytes[4] = b'0' + (count / 10) as u8;
            bytes[5] = b'0' + (count % 10) as u8;
            6
        } else {
            bytes[4] = b'0' + count as u8;
            5
        };
        Some(Self { bytes, length })
    }

    fn as_str(&self) -> &str {
        str::from_utf8(&self.bytes[..self.length])
            .expect("the mailbox badge formatter emits only ASCII")
    }
}

#[cfg(test)]
mod tests {
    use reticulum_appliance_display_model::{
        DisplayCommand, DisplayCompositionState, DisplayHomeSnapshot, DisplayLabel,
        DisplaySetupState, DisplayState,
    };

    use super::*;

    fn label() -> DisplayLabel {
        DisplayLabel::new("Reticulum E290").expect("fixture label fits")
    }

    fn home(setup: DisplaySetupState, lxmf: DisplayCompositionState) -> DisplayHomeSnapshot {
        DisplayHomeSnapshot::new(
            label(),
            DisplayLabel::new("e13f88").expect("fixture suffix fits"),
            setup,
            DisplayCompositionState::Configured,
            DisplayCompositionState::Configured,
            lxmf,
            DisplayCompositionState::Configured,
        )
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
            DisplayCommand::ShowHome {
                snapshot: home(
                    DisplaySetupState::EnrollmentRequired,
                    DisplayCompositionState::Configured,
                ),
            },
        ];

        for command in commands {
            let frame = render(command);
            assert!(frame.as_bytes().iter().any(|byte| *byte != 0));
            assert!(frame.as_bytes().iter().any(|byte| *byte != u8::MAX));
        }
    }

    #[test]
    fn home_changes_for_enrollment_state_and_boot_composition() {
        let enrollment_required = render(DisplayCommand::ShowHome {
            snapshot: home(
                DisplaySetupState::EnrollmentRequired,
                DisplayCompositionState::Configured,
            ),
        });
        let enrolled = render(DisplayCommand::ShowHome {
            snapshot: home(
                DisplaySetupState::Enrolled,
                DisplayCompositionState::Configured,
            ),
        });
        let lxmf_unavailable = render(DisplayCommand::ShowHome {
            snapshot: home(
                DisplaySetupState::Enrolled,
                DisplayCompositionState::Unavailable,
            ),
        });

        assert_ne!(enrollment_required.as_bytes(), enrolled.as_bytes());
        assert_ne!(enrolled.as_bytes(), lxmf_unavailable.as_bytes());
    }

    #[test]
    fn home_renders_a_bounded_uncollected_message_badge() {
        let base = home(
            DisplaySetupState::Enrolled,
            DisplayCompositionState::Configured,
        );
        let empty = render(DisplayCommand::ShowHome { snapshot: base });
        let one = render(DisplayCommand::ShowHome {
            snapshot: base.with_uncollected_messages(1),
        });
        let ninety_nine = render(DisplayCommand::ShowHome {
            snapshot: base.with_uncollected_messages(99),
        });
        let capped = render(DisplayCommand::ShowHome {
            snapshot: base.with_uncollected_messages(100),
        });
        let still_capped = render(DisplayCommand::ShowHome {
            snapshot: base.with_uncollected_messages(u32::MAX),
        });

        assert_ne!(empty.as_bytes(), one.as_bytes());
        assert_ne!(one.as_bytes(), ninety_nine.as_bytes());
        assert_ne!(ninety_nine.as_bytes(), capped.as_bytes());
        assert_eq!(capped.as_bytes(), still_capped.as_bytes());
    }

    #[test]
    fn named_home_renders_an_appliance_label_line_above_the_suffix() {
        let unnamed = render(DisplayCommand::ShowHome {
            snapshot: home(
                DisplaySetupState::Enrolled,
                DisplayCompositionState::Configured,
            ),
        });
        let named = render(DisplayCommand::ShowHome {
            snapshot: home(
                DisplaySetupState::Enrolled,
                DisplayCompositionState::Configured,
            )
            .with_appliance_label(Some(
                DisplayLabel::new("Field node").expect("fixture name fits"),
            )),
        });
        assert_ne!(unnamed.as_bytes(), named.as_bytes());
    }

    #[test]
    fn home_names_ble_as_a_reticulum_interface_in_every_link_variant() {
        for (lora, ble, expected) in [
            (
                DisplayCompositionState::Configured,
                DisplayCompositionState::Configured,
                "LORA NA915  RNS BLE",
            ),
            (
                DisplayCompositionState::Configured,
                DisplayCompositionState::Unavailable,
                "LORA NA915  RNS BLE --",
            ),
            (
                DisplayCompositionState::Unavailable,
                DisplayCompositionState::Configured,
                "LORA --  RNS BLE",
            ),
            (
                DisplayCompositionState::Unavailable,
                DisplayCompositionState::Unavailable,
                "LORA --  RNS BLE --",
            ),
        ] {
            let snapshot = DisplayHomeSnapshot::new(
                label(),
                DisplayLabel::new("e13f88").expect("fixture suffix fits"),
                DisplaySetupState::Enrolled,
                lora,
                ble,
                DisplayCompositionState::Configured,
                DisplayCompositionState::Configured,
            );
            assert_eq!(link_configuration(snapshot), expected);
        }
    }

    #[test]
    fn uncollected_badge_formatter_uses_new_and_caps_at_99_plus() {
        assert!(UncollectedBadgeText::new(0).is_none());
        for (value, expected) in [
            (1, "NEW 1"),
            (9, "NEW 9"),
            (10, "NEW 10"),
            (99, "NEW 99"),
            (100, "NEW 99+"),
            (u32::MAX, "NEW 99+"),
        ] {
            assert_eq!(
                UncollectedBadgeText::new(value)
                    .expect("positive count has a badge")
                    .as_str(),
                expected
            );
        }
    }
}
