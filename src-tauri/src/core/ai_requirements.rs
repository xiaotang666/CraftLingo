//! Template for the `ai翻译要求.txt` file written when the auto flow runs
//! without the LLM. Keeping the text in `assets/` makes it easy to edit without
//! touching Rust code.

const TEMPLATE: &str = include_str!("../../../assets/ai_translation_requirements.txt");

/// Render the requirements with the current target language filled in.
pub fn render(target_language: &str) -> String {
    TEMPLATE.replace("<target_language>", target_language)
}
