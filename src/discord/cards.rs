//! Building blocks for Discord's card layout ("components v2") and the
//! shared look: colours, toasts and error cards.

use twilight_model::channel::message::{
    Component, EmojiReactionType, MessageFlags,
    component::{
        ActionRow, Button, ButtonStyle, Container, Label, MediaGallery, MediaGalleryItem, Section,
        SelectMenu, SelectMenuOption, SelectMenuType, Separator, SeparatorSpacingSize, TextDisplay,
        TextInput, TextInputStyle, Thumbnail, UnfurledMediaItem,
    },
};

use super::icons::{Icon, Icons};

/// Every card message carries this flag; it can't be mixed with plain content or embeds.
pub const V2: MessageFlags = MessageFlags::IS_COMPONENTS_V2;

pub mod color {
    pub const BRAND: u32 = 0xF2A33A;
    pub const SUCCESS: u32 = 0x23A55A;
    pub const ERROR: u32 = 0xF23F43;
}

pub fn text(content: impl Into<String>) -> Component {
    Component::TextDisplay(TextDisplay {
        id: None,
        content: content.into(),
    })
}

/// Discord caps each text block at 2,000 characters; long lists are split
/// across as many blocks as they need.
const TEXT_BLOCK_MAX: usize = 1_900;

pub fn text_blocks(lines: impl IntoIterator<Item = String>) -> Vec<Component> {
    let mut blocks = Vec::new();
    let mut current = String::new();
    for line in lines {
        if !current.is_empty()
            && current.chars().count() + line.chars().count() + 1 > TEXT_BLOCK_MAX
        {
            blocks.push(text(std::mem::take(&mut current)));
        }
        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(&line);
    }
    if !current.is_empty() {
        blocks.push(text(current));
    }
    blocks
}

/// A thin line between parts of a card.
pub fn divider() -> Component {
    Component::Separator(Separator {
        id: None,
        divider: Some(true),
        spacing: Some(SeparatorSpacingSize::Small),
    })
}

fn media(url: &str) -> UnfurledMediaItem {
    UnfurledMediaItem {
        url: url.to_string(),
        proxy_url: None,
        height: None,
        width: None,
        content_type: None,
    }
}

pub fn thumbnail(url: &str) -> Component {
    Component::Thumbnail(Thumbnail {
        id: None,
        media: media(url),
        description: None,
        spoiler: None,
    })
}

/// One full-width image, with alt text for screen readers.
pub fn image(url: &str, alt: &str) -> Component {
    Component::MediaGallery(MediaGallery {
        id: None,
        items: vec![MediaGalleryItem {
            media: media(url),
            description: Some(alt.chars().take(1024).collect()),
            spoiler: None,
        }],
    })
}

/// A button that opens a web page.
pub fn link(url: &str, label: &str) -> Component {
    Component::Button(Button {
        id: None,
        custom_id: None,
        disabled: false,
        emoji: None,
        label: Some(label.to_string()),
        style: ButtonStyle::Link,
        url: Some(url.to_string()),
        sku_id: None,
    })
}

/// Text with something (usually a cover) on the right.
pub fn section(content: impl Into<String>, accessory: Component) -> Component {
    Component::Section(Section {
        id: None,
        components: vec![text(content)],
        accessory: Box::new(accessory),
    })
}

/// Text with an optional cover thumbnail beside it.
pub fn text_with_cover(content: impl Into<String>, cover: Option<&str>) -> Component {
    match cover {
        Some(url) => section(content, thumbnail(url)),
        None => text(content),
    }
}

pub fn card(accent: Option<u32>, components: Vec<Component>) -> Component {
    Component::Container(Container {
        id: None,
        accent_color: accent.map(Some),
        spoiler: None,
        components,
    })
}

pub fn row(components: Vec<Component>) -> Component {
    Component::ActionRow(ActionRow {
        id: None,
        components,
    })
}

pub struct Btn<'a> {
    pub id: &'a str,
    pub icon: Option<Icon>,
    pub label: Option<&'a str>,
    pub style: ButtonStyle,
    pub disabled: bool,
}

impl<'a> Btn<'a> {
    pub fn icon(id: &'a str, icon: Icon) -> Self {
        Self {
            id,
            icon: Some(icon),
            label: None,
            style: ButtonStyle::Secondary,
            disabled: false,
        }
    }

    pub fn label(id: &'a str, label: &'a str) -> Self {
        Self {
            id,
            icon: None,
            label: Some(label),
            style: ButtonStyle::Secondary,
            disabled: false,
        }
    }

    pub fn with_label(mut self, label: &'a str) -> Self {
        self.label = Some(label);
        self
    }

    pub fn style(mut self, style: ButtonStyle) -> Self {
        self.style = style;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn build(self, icons: &Icons) -> Component {
        Component::Button(Button {
            id: None,
            custom_id: Some(self.id.to_string()),
            disabled: self.disabled,
            emoji: self.icon.map(|i| icons.emoji(i)),
            label: self.label.map(str::to_string),
            style: self.style,
            url: None,
            sku_id: None,
        })
    }
}

pub fn select(
    id: &str,
    placeholder: &str,
    disabled: bool,
    options: Vec<SelectMenuOption>,
) -> Component {
    Component::SelectMenu(SelectMenu {
        id: None,
        channel_types: None,
        custom_id: id.to_string(),
        default_values: None,
        disabled,
        kind: SelectMenuType::Text,
        max_values: Some(1),
        min_values: Some(1),
        options: Some(options),
        placeholder: Some(placeholder.to_string()),
        required: None,
    })
}

pub fn option(
    label: impl Into<String>,
    value: impl Into<String>,
    description: Option<String>,
    emoji: Option<EmojiReactionType>,
) -> SelectMenuOption {
    SelectMenuOption {
        default: false,
        description,
        emoji,
        label: label.into(),
        value: value.into(),
    }
}

/// A labelled one-line text box for a pop-up form.
#[allow(deprecated)] // `TextInput::label` is the old way; the label wraps it instead.
pub fn text_field(
    custom_id: &str,
    label: &str,
    description: Option<&str>,
    placeholder: &str,
    value: Option<String>,
    max_length: u16,
) -> Component {
    Component::Label(Label {
        id: None,
        label: label.to_string(),
        description: description.map(str::to_string),
        component: Box::new(Component::TextInput(TextInput {
            id: None,
            custom_id: custom_id.to_string(),
            label: None,
            max_length: Some(max_length),
            min_length: Some(1),
            placeholder: Some(placeholder.to_string()),
            required: Some(true),
            style: TextInputStyle::Short,
            value,
        })),
    })
}

/// A one-line confirmation, optionally with the song's cover.
pub fn toast(icons: &Icons, message: &str, cover: Option<&str>, accent: u32) -> Component {
    let content = format!("{} {message}", icons.md(Icon::Done));
    card(Some(accent), vec![text_with_cover(content, cover)])
}

pub fn info(icons: &Icons, message: &str) -> Component {
    card(
        None,
        vec![text(format!("{} {message}", icons.md(Icon::Info)))],
    )
}

pub fn error(icons: &Icons, message: &str) -> Component {
    card(
        Some(color::ERROR),
        vec![text(format!("{} {message}", icons.md(Icon::Problem)))],
    )
}
