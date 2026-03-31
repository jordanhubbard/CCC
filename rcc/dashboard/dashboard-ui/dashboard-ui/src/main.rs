mod app;
mod components;
mod context;
mod types;

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount_to_body(app::App);
}
