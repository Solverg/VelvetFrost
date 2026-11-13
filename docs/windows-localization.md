# Windows localization

The Windows app and its installer use the tables in `locales/*.strings`, based on upstream
Snowhush v0.11.0. Windows' ordered preferred display languages are checked at process startup;
the first supported language wins, otherwise English is used. A missing key also falls back to
English. Restart the app after changing Windows' display-language preferences. Traditional
Chinese is not mapped to Simplified Chinese; another supported preferred language or English
is used instead.

The supported languages are English, German, Spanish, French, Japanese, Portuguese, Russian,
Ukrainian and Simplified Chinese. The Windows-only keys cover monitors, vehicles, their intervals,
the welcome title and installation errors. When adding a visible string, add the same key to every
table and keep the same named placeholders. Run `cargo test --manifest-path core/Cargo.toml
text::tests` to check table coverage, placeholders, language matching and fallback.

Mixed-mode menu captions use whole translated phrases rather than joining English names. The
installer and welcome card measure translated text before choosing their height. System error
messages supplied by Windows remain in the language provided by Windows.

The Halloween ghosts' sign captions are part of twelve pre-rendered images, not string tables;
they still display English. Localizing those requires language-specific artwork or a text-rendering
path that preserves their size and style.
