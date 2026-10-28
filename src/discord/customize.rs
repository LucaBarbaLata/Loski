//! /customize: make the bot your own. One card shows its banner, picture,
//! name and About me; forms change them, everywhere or in one server.

use std::time::Duration;

use anyhow::Result;
use twilight_interactions::command::{CommandModel, CreateCommand};
use twilight_model::{
    application::interaction::modal::{ModalInteractionComponent, ModalInteractionData},
    channel::{
        Attachment,
        message::{
            Component,
            component::{FileUpload, Label, TextInput, TextInputStyle},
        },
    },
    id::{
        Id,
        marker::{AttachmentMarker, GuildMarker, UserMarker},
    },
    util::ImageHash,
};

use super::{
    App,
    cards::{self, Btn, color},
    components::field,
    icons::{Icon, Icons},
    interact::Ix,
    oops,
    profile::{self, Image, Part, ServerLook},
};
use crate::util::{escape_md, truncate};

/// Discord takes profile pictures up to this size.
const MAX_IMAGE: u64 = 10 * 1024 * 1024;
const IMAGE_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "customize",
    desc = "Make Loski your own: its name, picture, banner and About me"
)]
pub struct CustomizeCmd;

/// What the card shows.
pub struct ProfileView {
    pub name: String,
    pub about: String,
    pub picture: String,
    pub banner: Option<String>,
    /// The name it goes back to.
    pub default_name: String,
    /// Its own look in the server the card is in, if any.
    pub here: Option<HereView>,
}

/// The bot's own look in one server.
#[derive(Default)]
pub struct HereView {
    pub nick: Option<String>,
    pub picture: bool,
    pub banner: bool,
}

impl HereView {
    fn is_plain(&self) -> bool {
        self.nick.is_none() && !self.picture && !self.banner
    }

    fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(nick) = &self.nick {
            parts.push(format!("called **{}**", escape_md(nick)));
        }
        if self.picture {
            parts.push("its own picture".to_string());
        }
        if self.banner {
            parts.push("its own banner".to_string());
        }
        if parts.is_empty() {
            "The same as everywhere. A nickname, picture, banner or bio here only shows in this server.".to_string()
        } else {
            let mut text = parts.join(", ");
            text[..1].make_ascii_uppercase();
            format!("{text}.")
        }
    }
}

impl ProfileView {
    async fn read(app: &App, guild: Option<Id<GuildMarker>>) -> Result<Self> {
        let (me, application, member) = tokio::join!(
            async {
                app.http
                    .current_user()
                    .await?
                    .model()
                    .await
                    .map_err(anyhow::Error::from)
            },
            async {
                app.http
                    .current_user_application()
                    .await?
                    .model()
                    .await
                    .map_err(anyhow::Error::from)
            },
            async {
                match guild {
                    Some(guild) => match app.http.guild_member(guild, app.bot_id).await {
                        Ok(response) => response.model().await.ok(),
                        Err(_) => None,
                    },
                    None => None,
                }
            },
        );
        let (me, application) = (me?, application?);
        Ok(Self {
            picture: picture_url(me.id, me.avatar),
            banner: me.banner.map(|hash| banner_url(me.id, hash)),
            name: me.name,
            about: application.description,
            default_name: app.default_name(),
            here: guild.map(|_| {
                member
                    .map(|m| HereView {
                        nick: m.nick,
                        picture: m.avatar.is_some(),
                        banner: m.banner.is_some(),
                    })
                    .unwrap_or_default()
            }),
        })
    }
}

fn picture_url(user: Id<UserMarker>, hash: Option<ImageHash>) -> String {
    match hash {
        Some(hash) => format!(
            "https://cdn.discordapp.com/avatars/{user}/{hash}.{}?size=256",
            if hash.is_animated() { "gif" } else { "png" }
        ),
        None => format!(
            "https://cdn.discordapp.com/embed/avatars/{}.png",
            (user.get() >> 22) % 6
        ),
    }
}

fn banner_url(user: Id<UserMarker>, hash: ImageHash) -> String {
    format!(
        "https://cdn.discordapp.com/banners/{user}/{hash}.{}?size=1024",
        if hash.is_animated() { "gif" } else { "png" }
    )
}

pub fn customize_card(icons: &Icons, view: &ProfileView) -> Component {
    let mut parts = Vec::new();
    if let Some(banner) = &view.banner {
        parts.push(cards::gallery(&[(banner.as_str(), "Banner")]));
    }
    let about = if view.about.trim().is_empty() {
        "-# No About me yet.".to_string()
    } else {
        escape_md(&truncate(&view.about, 400))
    };
    parts.push(cards::section(
        format!("### {}\n{about}", escape_md(&view.name)),
        cards::thumbnail(&view.picture),
    ));
    parts.push(cards::row(vec![
        Btn::icon("loski:me:edit", Icon::Settings)
            .with_label("Edit profile")
            .build(icons),
    ]));
    parts.push(cards::row(vec![cards::select(
        "loski:me:reset",
        "Back to Loski's own…",
        false,
        vec![
            cards::option(
                "Name",
                "name",
                Some(format!(
                    "Call it {} again",
                    truncate(&view.default_name, 60)
                )),
                None,
            ),
            cards::option(
                "Picture",
                "picture",
                Some("Loski's own picture".into()),
                None,
            ),
            cards::option("Banner", "banner", Some("Loski's own banner".into()), None),
            cards::option("About me", "about", Some("Loski's own words".into()), None),
            cards::option("Everything", "all", Some("All four at once".into()), None),
        ],
    )]));
    let close = Btn::label("loski:me:close", "Close").build(icons);
    match &view.here {
        Some(here) => {
            parts.push(cards::divider());
            parts.push(cards::text(format!(
                "-# IN THIS SERVER\n{}",
                here.summary()
            )));
            parts.push(cards::row(vec![
                Btn::icon("loski:me:here", Icon::Friends)
                    .with_label("Edit for this server")
                    .build(icons),
                Btn::label("loski:me:plain", "Same as everywhere")
                    .disabled(here.is_plain())
                    .build(icons),
                close,
            ]));
        }
        None => parts.push(cards::row(vec![close])),
    }
    cards::card(Some(color::BRAND), parts)
}

pub async fn customize(ix: &Ix, _: CustomizeCmd) -> Result<()> {
    ix.defer_ephemeral().await?;
    let view = ProfileView::read(&ix.app, ix.guild_id).await?;
    ix.reply_private(vec![customize_card(&ix.app.icons, &view)])
        .await
}

/// Puts the card back up to date, after a change that was deferred.
async fn refresh(ix: &Ix) -> Result<()> {
    let view = ProfileView::read(&ix.app, ix.guild_id).await?;
    ix.update_message(vec![customize_card(&ix.app.icons, &view)])
        .await
}

/// A button or menu on the card; `key` is what follows `loski:me:`.
pub async fn change(ix: &Ix, key: &str, value: Option<&str>) -> Result<()> {
    let app = &ix.app;
    match key {
        "edit" => {
            let about = match app.http.current_user_application().await {
                Ok(response) => response
                    .model()
                    .await
                    .map(|a| a.description)
                    .unwrap_or_default(),
                Err(_) => String::new(),
            };
            ix.modal(
                "loski:m:me",
                "Edit profile",
                vec![
                    input(
                        "name",
                        "Name",
                        "Discord allows two name changes an hour.",
                        Some(app.name()),
                        (2, 32),
                        TextInputStyle::Short,
                    ),
                    input(
                        "about",
                        "About me",
                        "Shown on its profile. Leave it empty for Loski's own words.",
                        Some(truncate(&about, 400)).filter(|a| !a.is_empty()),
                        (0, 400),
                        TextInputStyle::Paragraph,
                    ),
                    upload(
                        "picture",
                        "Picture",
                        "PNG, JPG, GIF or WebP. Square works best. Empty keeps this one.",
                    ),
                    upload(
                        "banner",
                        "Banner",
                        "Wide works best, like 680 × 240. Empty keeps this one.",
                    ),
                ],
            )
            .await
        }
        "here" => {
            let Some(guild) = ix.guild_id else {
                return ix.ack().await;
            };
            let nick = app
                .cache
                .member(guild, app.bot_id)
                .and_then(|m| m.nick().map(str::to_string));
            ix.modal(
                "loski:m:here",
                "In this server",
                vec![
                    input(
                        "nick",
                        "Nickname",
                        &format!("Empty: {}, its name everywhere.", truncate(&app.name(), 40)),
                        nick,
                        (0, 32),
                        TextInputStyle::Short,
                    ),
                    input(
                        "bio",
                        "Bio",
                        "Shown on its profile in this server. Empty keeps what's there.",
                        None,
                        (0, 190),
                        TextInputStyle::Paragraph,
                    ),
                    upload(
                        "picture",
                        "Picture",
                        "Only in this server. Empty keeps what's there.",
                    ),
                    upload(
                        "banner",
                        "Banner",
                        "Only in this server. Empty keeps what's there.",
                    ),
                ],
            )
            .await
        }
        "reset" => {
            let part = match value {
                Some("name") => Part::Name,
                Some("picture") => Part::Picture,
                Some("banner") => Part::Banner,
                Some("about") => Part::About,
                Some("all") => Part::All,
                _ => return ix.ack().await,
            };
            ix.defer_update().await?;
            let result = profile::reset(app, part).await;
            refresh(ix).await?;
            result
        }
        "plain" => {
            let Some(guild) = ix.guild_id else {
                return ix.ack().await;
            };
            ix.defer_update().await?;
            let result = profile::reset_server(app, guild).await;
            refresh(ix).await?;
            result
        }
        "close" => {
            ix.ack().await?;
            ix.delete_reply().await;
            Ok(())
        }
        _ => ix.ack().await,
    }
}

/// "Edit profile" came back. Pictures go first, so a name Discord turns
/// down (two renames an hour) doesn't hold them up.
pub async fn edited(ix: &Ix, data: &ModalInteractionData) -> Result<()> {
    ix.defer_update().await?;
    let app = &ix.app;
    let mut problems = Vec::new();
    if let Some(sent) = sent_file(data, "picture")
        && let Err(e) = async {
            let image = download(app, &sent, "picture").await?;
            profile::set_picture(app, &image).await
        }
        .await
    {
        problems.push(e);
    }
    if let Some(sent) = sent_file(data, "banner")
        && let Err(e) = async {
            let image = download(app, &sent, "banner").await?;
            profile::set_banner(app, &image).await
        }
        .await
    {
        problems.push(e);
    }
    let about = field(&data.components, "about").unwrap_or_default();
    let current = match app.http.current_user_application().await {
        Ok(response) => response.model().await.map(|a| a.description).ok(),
        Err(_) => None,
    };
    let unchanged = current.as_deref().is_some_and(|c| c.trim() == about.trim())
        || (about.is_empty() && current.as_deref() == Some(profile::ABOUT));
    if !unchanged && let Err(e) = profile::set_about(app, &about).await {
        problems.push(e);
    }
    let name = field(&data.components, "name").unwrap_or_default();
    if !name.is_empty()
        && name != app.name()
        && let Err(e) = profile::set_name(app, &name).await
    {
        problems.push(e);
    }
    refresh(ix).await?;
    finish(problems)
}

/// "In this server" came back.
pub async fn edited_here(ix: &Ix, data: &ModalInteractionData) -> Result<()> {
    let Some(guild) = ix.guild_id else {
        return ix.ack().await;
    };
    ix.defer_update().await?;
    let app = &ix.app;
    let mut problems = Vec::new();
    let mut look = ServerLook::default();
    for (id, slot) in [("picture", &mut look.picture), ("banner", &mut look.banner)] {
        if let Some(sent) = sent_file(data, id) {
            match download(app, &sent, id).await {
                Ok(image) => *slot = Some(image),
                Err(e) => problems.push(e),
            }
        }
    }
    let bio = field(&data.components, "bio").unwrap_or_default();
    look.bio = (!bio.is_empty()).then_some(bio);
    let nick = field(&data.components, "nick").unwrap_or_default();
    let current = app
        .cache
        .member(guild, app.bot_id)
        .and_then(|m| m.nick().map(str::to_string))
        .unwrap_or_default();
    look.nick = (nick != current).then_some(nick);
    if let Err(e) = profile::set_server(app, guild, &look).await {
        problems.push(e);
    }
    refresh(ix).await?;
    finish(problems)
}

/// Whatever didn't work, said once.
fn finish(problems: Vec<anyhow::Error>) -> Result<()> {
    if problems.is_empty() {
        return Ok(());
    }
    let said: Vec<String> = problems.iter().map(|e| e.to_string()).collect();
    Err(oops(said.join("\n")))
}

/// The first file sent with a form's upload field.
fn sent_file(data: &ModalInteractionData, id: &str) -> Option<Attachment> {
    fn find(
        components: &[ModalInteractionComponent],
        id: &str,
    ) -> Option<Vec<Id<AttachmentMarker>>> {
        components.iter().find_map(|component| match component {
            ModalInteractionComponent::FileUpload(upload) if upload.custom_id == id => {
                Some(upload.values.clone())
            }
            ModalInteractionComponent::Label(label) => {
                find(std::slice::from_ref(&label.component), id)
            }
            ModalInteractionComponent::ActionRow(row) => find(&row.components, id),
            _ => None,
        })
    }
    let attachment = *find(&data.components, id)?.first()?;
    data.resolved
        .as_ref()?
        .attachments
        .get(&attachment)
        .cloned()
}

/// Fetches a picture sent with a form, after checking it's one Discord takes.
async fn download(app: &App, file: &Attachment, what: &str) -> Result<Image> {
    let mime = file
        .content_type
        .as_deref()
        .and_then(|t| t.split(';').next())
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if !IMAGE_TYPES.contains(&mime.as_str()) {
        return Err(oops(format!(
            "The {what} has to be a PNG, JPG, GIF or WebP picture."
        )));
    }
    if file.size > MAX_IMAGE {
        return Err(oops(format!(
            "The {what} is over 10 MB. Try a smaller one."
        )));
    }
    let bytes = app
        .web
        .get(&file.url)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| oops(format!("I couldn't fetch the {what} from Discord: {e}")))?
        .bytes()
        .await?;
    Ok(Image {
        bytes: bytes.to_vec(),
        mime,
    })
}

/// A labelled text box; `min` 0 makes it optional.
#[allow(deprecated)] // `TextInput::label` is the old way; the label wraps it instead.
fn input(
    custom_id: &str,
    label: &str,
    description: &str,
    value: Option<String>,
    (min, max): (u16, u16),
    style: TextInputStyle,
) -> Component {
    Component::Label(Label {
        id: None,
        label: label.to_string(),
        description: Some(description.to_string()),
        component: Box::new(Component::TextInput(TextInput {
            id: None,
            custom_id: custom_id.to_string(),
            label: None,
            max_length: Some(max),
            min_length: (min > 0).then_some(min),
            placeholder: None,
            required: Some(min > 0),
            style,
            value,
        })),
    })
}

/// A labelled field for one optional picture.
fn upload(custom_id: &str, label: &str, description: &str) -> Component {
    Component::Label(Label {
        id: None,
        label: label.to_string(),
        description: Some(description.to_string()),
        component: Box::new(Component::FileUpload(FileUpload {
            id: None,
            custom_id: custom_id.to_string(),
            max_values: Some(1),
            min_values: Some(0),
            required: Some(false),
        })),
    })
}
