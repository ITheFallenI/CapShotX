#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

#[allow(dead_code)]
mod commands;
#[allow(dead_code)]
mod icon;
mod image_editor;
#[allow(dead_code)]
mod screen_record {
    pub struct Active;
    impl std::fmt::Debug for Active {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Active")
        }
    }
}
#[allow(dead_code)]
mod settings;
#[allow(dead_code)]
mod theme;

fn main() -> anyhow::Result<()> {
    platform_windows::shell::init_process();

    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    let settings = settings::Settings::load();

    let mut editor = None;
    let mut i = 0;
    while i < raw_args.len() {
        match raw_args[i].as_str() {
            "--editor" => {
                editor = Some(image_editor::ImageEditor::new_placeholder());
            }
            "--editor-paste" => {
                let mut ed = image_editor::ImageEditor::new_placeholder();
                ed.paste_image_from_clipboard();
                editor = Some(ed);
            }
            "--editor-file" => {
                if let Some(p) = raw_args.get(i + 1) {
                    let path = PathBuf::from(p);
                    editor = Some(
                        image_editor::ImageEditor::from_path(&path)
                            .unwrap_or_else(|_| image_editor::ImageEditor::new_placeholder()),
                    );
                    i += 1;
                }
            }
            "--editor-bgra" => {
                if let Some(p) = raw_args.get(i + 1) {
                    let path = PathBuf::from(p);
                    editor = Some(
                        image_editor::ImageEditor::from_temp_bgra_file(&path)
                            .unwrap_or_else(|_| image_editor::ImageEditor::new_placeholder()),
                    );
                    i += 1;
                }
            }
            other if !other.starts_with('-') => {
                let path = PathBuf::from(other);
                if path.exists() {
                    editor = Some(
                        image_editor::ImageEditor::from_path(&path)
                            .unwrap_or_else(|_| image_editor::ImageEditor::new_placeholder()),
                    );
                }
            }
            _ => {}
        }
        i += 1;
    }

    let editor = editor.unwrap_or_else(image_editor::ImageEditor::new_placeholder);
    image_editor::run_editor_window(settings, editor)
}
