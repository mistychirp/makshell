mod alert;
mod anim;
mod app;
mod appearance;
mod appmenu;
mod apps;
mod arrange;
mod audio;
mod autostart;
mod banners;
mod bluetooth;
mod brightness;
mod clipboard;
mod config;
mod control;
mod dbus;
mod dbusmenu;
mod desktop;
mod dnd;
mod dock;
mod fan;
mod fonts;
mod glass_protocol;
mod group;
mod hud;
mod icon_themes;
mod icons;
mod kwin;
mod launcher;
mod material;
mod media;
mod menu;
mod menubar;
mod migrate;
mod network;
mod notification_center;
mod notifications;
mod places;
mod plasma_protocol;
mod portal;
mod running;
mod settings;
mod settings_file;
mod shadow_protocol;
mod spotlight;
mod tray;
mod wallpaper;
mod widgets;
mod window_shadow;

fn main() -> Result<(), iced_layershell::Error> {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("warn,layershellev=error,makshell=info"),
    )
    .init();
    migrate::from_old_name();
    icon_themes::note_start();
    fonts::load();
    appearance::init();
    if std::env::args().nth(1).as_deref() == Some("settings") {
        return settings::run(std::env::args().nth(2).as_deref());
    }
    app::run()
}
