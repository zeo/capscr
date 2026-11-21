use std::path::Path;

fn main() {
    ceri_build::embed("frontend/dist");
    ceri_build::resources(ceri_build::Resources {
        icon: Some(Path::new("icons/icon.ico")),
        ..Default::default()
    });
}
