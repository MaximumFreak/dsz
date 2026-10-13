mod haptics;
mod input;
mod lighting;
mod mappings;
mod overview;
mod profiles;
mod settings;
mod triggers;
pub mod virtual_pad;

use ds_proto::input::Button;

/// Buttons offered in pickers. Touchpad zones sit with Touchpad; Edge-only
/// buttons come last and only when `edge` is set.
pub fn button_choices(edge: bool) -> Vec<Button> {
    let mut out: Vec<Button> = Button::ALL
        .iter()
        .copied()
        .filter(|b| !b.is_touchpad_zone() && (edge || !b.edge_only()))
        .collect();
    if let Some(i) = out.iter().position(|b| *b == Button::Touchpad) {
        out.splice(
            i + 1..i + 1,
            [
                Button::TouchpadLeft,
                Button::TouchpadCenter,
                Button::TouchpadRight,
            ],
        );
    }
    out
}
