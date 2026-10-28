//! One handle per interaction (command, button, autocomplete) that knows
//! whether it has replied yet, so every handler can just say "reply with
//! this card" and have it land in the right place.

use std::{sync::Arc, time::Duration};

use anyhow::Result;
use parking_lot::Mutex;
use twilight_http::client::InteractionClient;
use twilight_model::{
    application::{
        command::CommandOptionChoice,
        interaction::{Interaction, InteractionType},
    },
    channel::message::{Component, MessageFlags},
    http::interaction::{InteractionResponse, InteractionResponseData, InteractionResponseType},
    id::{
        Id,
        marker::{ChannelMarker, GuildMarker, InteractionMarker, RoleMarker, UserMarker},
    },
};
use twilight_util::builder::InteractionResponseDataBuilder;

use super::{
    App, Oops, cards,
    friends::{self, Access},
};
use crate::personality;

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Fresh,
    Deferred,
    Replied,
}

pub struct Ix {
    pub app: Arc<App>,
    pub id: Id<InteractionMarker>,
    pub token: String,
    pub guild_id: Option<Id<GuildMarker>>,
    pub channel_id: Option<Id<ChannelMarker>>,
    pub user_id: Option<Id<UserMarker>>,
    /// What the server calls whoever this is (nickname, display name or username).
    pub user_name: String,
    /// Their roles in this server, for the DJ role.
    roles: Vec<Id<RoleMarker>>,
    /// A button or menu: its "reply" is the message it sits on.
    from_component: bool,
    state: Mutex<State>,
}

impl Ix {
    pub fn new(app: Arc<App>, interaction: &Interaction) -> Self {
        Self {
            app,
            id: interaction.id,
            token: interaction.token.clone(),
            guild_id: interaction.guild_id,
            channel_id: interaction.channel.as_ref().map(|c| c.id),
            user_id: interaction.author_id(),
            user_name: display_name(interaction),
            roles: interaction
                .member
                .as_ref()
                .map(|m| m.roles.clone())
                .unwrap_or_default(),
            // A form opened from a button belongs to that button's message too.
            from_component: interaction.kind == InteractionType::MessageComponent
                || (interaction.kind == InteractionType::ModalSubmit
                    && interaction.message.is_some()),
            state: Mutex::new(State::Fresh),
        }
    }

    /// A button, menu or a form opened from one (as opposed to a command).
    pub fn is_component(&self) -> bool {
        self.from_component
    }

    pub fn is_owner(&self) -> bool {
        self.user_id.is_some_and(|u| u == self.app.owner_id())
    }

    /// What this person may do: everything for you, what you allowed for a
    /// co-DJ, listening along for everyone else.
    pub fn access(&self) -> Access {
        if self.is_owner() {
            return Access::Owner;
        }
        let prefs = self.app.prefs();
        match self.user_id {
            Some(user) if friends::is_co_dj(&prefs, user.get(), &self.roles) => {
                friends::co_dj_access(&prefs)
            }
            _ => Access::Listener,
        }
    }

    fn client(&self) -> InteractionClient<'_> {
        self.app.http.interaction(self.app.app_id)
    }

    async fn respond(
        &self,
        kind: InteractionResponseType,
        data: Option<InteractionResponseData>,
    ) -> Result<()> {
        self.client()
            .create_response(self.id, &self.token, &InteractionResponse { kind, data })
            .await?;
        Ok(())
    }

    /// Shows "Loski is thinking…" so slow work (searching, joining voice) has time.
    pub async fn defer(&self) -> Result<()> {
        self.respond(
            InteractionResponseType::DeferredChannelMessageWithSource,
            None,
        )
        .await?;
        *self.state.lock() = State::Deferred;
        Ok(())
    }

    /// For a button or form whose work takes a while: Discord keeps
    /// waiting, and `update_message` fills in the card when it's done.
    pub async fn defer_update(&self) -> Result<()> {
        self.respond(InteractionResponseType::DeferredUpdateMessage, None)
            .await?;
        *self.state.lock() = State::Deferred;
        Ok(())
    }

    pub async fn defer_ephemeral(&self) -> Result<()> {
        let data = InteractionResponseDataBuilder::new()
            .flags(MessageFlags::EPHEMERAL)
            .build();
        self.respond(
            InteractionResponseType::DeferredChannelMessageWithSource,
            Some(data),
        )
        .await?;
        *self.state.lock() = State::Deferred;
        Ok(())
    }

    /// Replies with a card (public), or fills in the deferred reply.
    pub async fn reply(&self, components: Vec<Component>) -> Result<()> {
        self.reply_with(components, false).await
    }

    /// Replies with a card only the person who clicked can see.
    pub async fn reply_private(&self, components: Vec<Component>) -> Result<()> {
        self.reply_with(components, true).await
    }

    async fn reply_with(&self, components: Vec<Component>, private: bool) -> Result<()> {
        let state = *self.state.lock();
        match state {
            State::Fresh => {
                let mut flags = cards::V2;
                if private {
                    flags |= MessageFlags::EPHEMERAL;
                }
                let data = InteractionResponseDataBuilder::new()
                    .flags(flags)
                    .components(components)
                    .build();
                self.respond(
                    InteractionResponseType::ChannelMessageWithSource,
                    Some(data),
                )
                .await?;
            }
            State::Deferred | State::Replied => {
                self.client()
                    .update_response(&self.token)
                    .flags(cards::V2)
                    .components(Some(&components))
                    .await?;
            }
        }
        *self.state.lock() = State::Replied;
        Ok(())
    }

    /// Public reply that removes itself after `secs`.
    pub async fn reply_temp(&self, components: Vec<Component>, secs: u64) -> Result<()> {
        self.reply(components).await?;
        self.delete_later(secs);
        Ok(())
    }

    /// Replaces the message a button or menu was on.
    pub async fn update_message(&self, components: Vec<Component>) -> Result<()> {
        if *self.state.lock() == State::Deferred {
            self.client()
                .update_response(&self.token)
                .flags(cards::V2)
                .components(Some(&components))
                .await?;
            *self.state.lock() = State::Replied;
            return Ok(());
        }
        let data = InteractionResponseDataBuilder::new()
            .flags(cards::V2)
            .components(components)
            .build();
        self.respond(InteractionResponseType::UpdateMessage, Some(data))
            .await?;
        *self.state.lock() = State::Replied;
        Ok(())
    }

    /// Changes what this interaction already showed (its reply, or the card
    /// its button is on), after the first answer.
    pub async fn edit(&self, components: Vec<Component>) -> Result<()> {
        self.client()
            .update_response(&self.token)
            .flags(cards::V2)
            .components(Some(&components))
            .await?;
        Ok(())
    }

    /// A separate message under this one, removed after `secs`. For buttons
    /// that leave their own card as it is.
    pub async fn followup_temp(&self, components: Vec<Component>, secs: u64) -> Result<()> {
        let message = self
            .client()
            .create_followup(&self.token)
            .flags(cards::V2)
            .components(&components)
            .await?
            .model()
            .await?;
        let app = self.app.clone();
        let token = self.token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(secs)).await;
            let _ = app
                .http
                .interaction(app.app_id)
                .delete_followup(&token, message.id)
                .await;
        });
        Ok(())
    }

    /// Opens a pop-up form. It has to be the first answer to a button or command.
    pub async fn modal(&self, custom_id: &str, title: &str, fields: Vec<Component>) -> Result<()> {
        let data = InteractionResponseDataBuilder::new()
            .custom_id(custom_id)
            .title(title)
            .components(fields)
            .build();
        self.respond(InteractionResponseType::Modal, Some(data))
            .await?;
        *self.state.lock() = State::Replied;
        Ok(())
    }

    /// Acknowledges a button press without changing its message.
    pub async fn ack(&self) -> Result<()> {
        self.respond(InteractionResponseType::DeferredUpdateMessage, None)
            .await?;
        *self.state.lock() = State::Replied;
        Ok(())
    }

    pub async fn autocomplete(&self, choices: Vec<CommandOptionChoice>) -> Result<()> {
        let data = InteractionResponseDataBuilder::new()
            .choices(choices)
            .build();
        self.respond(
            InteractionResponseType::ApplicationCommandAutocompleteResult,
            Some(data),
        )
        .await
    }

    /// Deletes this interaction's reply (or the message a button was on).
    pub async fn delete_reply(&self) {
        let _ = self.client().delete_response(&self.token).await;
    }

    pub fn delete_later(&self, secs: u64) {
        let app = self.app.clone();
        let token = self.token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(secs)).await;
            let _ = app
                .http
                .interaction(app.app_id)
                .delete_response(&token)
                .await;
        });
    }

    /// Shows an error card. Messages from `Oops` are shown as written;
    /// anything else is logged and summarised.
    pub async fn fail(&self, error: anyhow::Error) {
        let message = match error.downcast_ref::<Oops>() {
            Some(oops) => oops.0.clone(),
            None => {
                tracing::error!("interaction failed: {error:#}");
                format!("Something went wrong: {error}")
            }
        };
        let card = cards::error(&self.app.icons, &message);
        let state = *self.state.lock();
        let result = if self.from_component && state != State::Fresh {
            // Editing "the reply" here would overwrite the panel or card the
            // button was on, so the error goes in a separate private message.
            self.client()
                .create_followup(&self.token)
                .flags(cards::V2 | MessageFlags::EPHEMERAL)
                .components(&[card])
                .await
                .map(|_| ())
                .map_err(Into::into)
        } else if state == State::Fresh && !self.is_owner() {
            self.reply_private(vec![card]).await
        } else {
            self.reply_temp(vec![card], super::ERROR_SECS).await
        };
        if let Err(e) = result {
            tracing::warn!("could not show an error: {e:#}");
        }
    }

    /// The polite refusal for something this person may not do.
    pub async fn refuse(&self) -> Result<()> {
        let text = match self.access() {
            Access::Queue => {
                "As a co-DJ you can add songs and look at the queue. The rest stays with whoever runs me.".to_string()
            }
            Access::Full => "That one stays with whoever runs me.".to_string(),
            _ => format!(
                "{}\n-# You can still suggest a song with `/request`, like songs and vote to skip.",
                personality::not_owner()
            ),
        };
        let card = cards::card(None, vec![cards::text(text)]);
        self.reply_private(vec![card]).await
    }
}

fn display_name(interaction: &Interaction) -> String {
    let user = interaction
        .member
        .as_ref()
        .and_then(|m| m.user.as_ref())
        .or(interaction.user.as_ref());
    interaction
        .member
        .as_ref()
        .and_then(|m| m.nick.clone())
        .or_else(|| user.and_then(|u| u.global_name.clone()))
        .or_else(|| user.map(|u| u.name.clone()))
        .unwrap_or_else(|| "Someone".to_string())
}
