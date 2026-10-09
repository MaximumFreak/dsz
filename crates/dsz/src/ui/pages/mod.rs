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

/// Buttons offered in pickers, Edge-only ones last.
pub fn button_choices(edge: bool) -> Vec<Button> {
    Button::ALL
        .iter()
        .copied()
        .filter(|b| edge || !b.edge_only())
        .collect()
}
