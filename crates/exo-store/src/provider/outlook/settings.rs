//! Outlook mailbox settings: reading and updating `mailboxSettings.automaticRepliesSetting`
//! through the provider-neutral [`AutoReply`] type.

use jiff::Timestamp;
use jiff::tz::TimeZone;
use outlook::DateTimeTimeZone;
use outlook::settings::{
    AutomaticRepliesSetting, AutomaticRepliesStatus, ExternalAudienceScope, MailboxSettings,
};

use super::Outlook;
use crate::error::{Error, Result};
use crate::out_of_office::{AutoReply, AutoReplyStatus, ExternalAudience, OutlookAutoReply};
use crate::quota::Lane;

/// Attempts when reading automatic replies.
const READ_ATTEMPTS: u32 = 3;

impl Outlook {
    /// Reads the mailbox's automatic replies (`GET /me/mailboxSettings`).
    pub(super) async fn auto_reply(&self) -> Result<AutoReply> {
        let settings = self
            .graph
            .call(Lane::Act, READ_ATTEMPTS, |graph| {
                graph
                    .settings()
                    .mailbox_settings()
                    .select("automaticRepliesSetting")
            })
            .await?;
        let reply = settings
            .automatic_replies_setting
            .as_ref()
            .map(from_graph)
            .unwrap_or_default();
        Ok(AutoReply::Outlook(reply))
    }

    /// Updates the mailbox's automatic replies (`PATCH /me/mailboxSettings`).
    pub(super) async fn update_auto_reply(&self, reply: &AutoReply) -> Result<AutoReply> {
        let AutoReply::Outlook(reply) = reply else {
            return Err(Error::Unsupported("Gmail vacation settings"));
        };
        let patch = MailboxSettings {
            automatic_replies_setting: Some(to_graph(reply)),
            ..MailboxSettings::default()
        };
        let saved = self
            .graph
            .call(Lane::Act, 1, |graph| {
                graph.settings().update_mailbox_settings(&patch)
            })
            .await?;
        let reply = saved
            .automatic_replies_setting
            .as_ref()
            .map(from_graph)
            .unwrap_or_else(|| reply.clone());
        Ok(AutoReply::Outlook(reply))
    }
}

/// Converts Graph's `automaticRepliesSetting` to [`OutlookAutoReply`]. Scheduled timestamps are
/// only populated when `status` is [`AutoReplyStatus::Scheduled`], because Graph returns default
/// placeholder dates even while automatic replies are disabled or always enabled.
fn from_graph(setting: &AutomaticRepliesSetting) -> OutlookAutoReply {
    let status = match setting.status {
        Some(AutomaticRepliesStatus::AlwaysEnabled) => AutoReplyStatus::AlwaysEnabled,
        Some(AutomaticRepliesStatus::Scheduled) => AutoReplyStatus::Scheduled,
        Some(AutomaticRepliesStatus::Disabled | AutomaticRepliesStatus::UnknownFutureValue)
        | None => AutoReplyStatus::Disabled,
    };
    let external_audience = match setting.external_audience {
        Some(ExternalAudienceScope::ContactsOnly) => ExternalAudience::ContactsOnly,
        Some(ExternalAudienceScope::All) => ExternalAudience::All,
        Some(ExternalAudienceScope::None | ExternalAudienceScope::UnknownFutureValue) | None => {
            ExternalAudience::None
        }
    };
    let (start_time, end_time) = if status == AutoReplyStatus::Scheduled {
        (
            setting
                .scheduled_start_date_time
                .as_ref()
                .and_then(parse_date_time_zone),
            setting
                .scheduled_end_date_time
                .as_ref()
                .and_then(parse_date_time_zone),
        )
    } else {
        (None, None)
    };
    OutlookAutoReply {
        status,
        external_audience,
        internal_reply_html: setting.internal_reply_message.clone().unwrap_or_default(),
        external_reply_html: setting.external_reply_message.clone().unwrap_or_default(),
        start_time,
        end_time,
    }
}

/// Converts [`OutlookAutoReply`] to Graph's `automaticRepliesSetting`.
fn to_graph(reply: &OutlookAutoReply) -> AutomaticRepliesSetting {
    let status = match reply.status {
        AutoReplyStatus::Disabled => AutomaticRepliesStatus::Disabled,
        AutoReplyStatus::AlwaysEnabled => AutomaticRepliesStatus::AlwaysEnabled,
        AutoReplyStatus::Scheduled => AutomaticRepliesStatus::Scheduled,
    };
    let external_audience = match reply.external_audience {
        ExternalAudience::None => ExternalAudienceScope::None,
        ExternalAudience::ContactsOnly => ExternalAudienceScope::ContactsOnly,
        ExternalAudience::All => ExternalAudienceScope::All,
    };
    let (scheduled_start_date_time, scheduled_end_date_time) =
        if reply.status == AutoReplyStatus::Scheduled {
            (
                reply.start_time.and_then(format_utc_date_time_zone),
                reply.end_time.and_then(format_utc_date_time_zone),
            )
        } else {
            (None, None)
        };
    AutomaticRepliesSetting {
        status: Some(status),
        external_audience: Some(external_audience),
        internal_reply_message: Some(reply.internal_reply_html.clone()),
        external_reply_message: Some(reply.external_reply_html.clone()),
        scheduled_start_date_time,
        scheduled_end_date_time,
    }
}

/// Parses a Graph `dateTimeTimeZone` (wall time + Windows or IANA zone) into epoch milliseconds.
fn parse_date_time_zone(value: &DateTimeTimeZone) -> Option<i64> {
    let tz = gmail::ics::resolve_timezone(&value.time_zone).unwrap_or(TimeZone::UTC);
    value
        .date_time
        .to_zoned(tz)
        .ok()
        .map(|zoned| zoned.timestamp().as_millisecond())
}

/// Formats epoch milliseconds as a UTC `dateTimeTimeZone` for Graph.
fn format_utc_date_time_zone(ms: i64) -> Option<DateTimeTimeZone> {
    let instant = Timestamp::from_millisecond(ms).ok()?;
    Some(DateTimeTimeZone {
        date_time: instant.to_zoned(TimeZone::UTC).datetime(),
        time_zone: "UTC".to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_scheduled_and_disabled_graph_settings_cleanly() {
        let start_ms = "2026-11-20T14:00:00Z"
            .parse::<Timestamp>()
            .unwrap()
            .as_millisecond();
        let end_ms = "2026-11-25T22:00:00Z"
            .parse::<Timestamp>()
            .unwrap()
            .as_millisecond();

        let graph = AutomaticRepliesSetting {
            status: Some(AutomaticRepliesStatus::Scheduled),
            external_audience: Some(ExternalAudienceScope::ContactsOnly),
            internal_reply_message: Some("<p>Internal</p>".into()),
            external_reply_message: Some("<p>External</p>".into()),
            scheduled_start_date_time: Some(DateTimeTimeZone {
                date_time: "2026-11-20T09:00:00".parse().unwrap(),
                time_zone: "Eastern Standard Time".into(),
            }),
            scheduled_end_date_time: Some(DateTimeTimeZone {
                date_time: "2026-11-25T22:00:00".parse().unwrap(),
                time_zone: "UTC".into(),
            }),
        };
        let converted = from_graph(&graph);
        assert_eq!(converted.status, AutoReplyStatus::Scheduled);
        assert_eq!(converted.external_audience, ExternalAudience::ContactsOnly);
        assert_eq!(converted.start_time, Some(start_ms));
        assert_eq!(converted.end_time, Some(end_ms));

        let round_tripped = from_graph(&to_graph(&converted));
        assert_eq!(round_tripped, converted);

        // Disabled status ignores Graph's default placeholder timestamps so polling stays stable.
        let disabled = AutomaticRepliesSetting {
            status: Some(AutomaticRepliesStatus::Disabled),
            ..graph
        };
        let off = from_graph(&disabled);
        assert_eq!(off.status, AutoReplyStatus::Disabled);
        assert_eq!(off.start_time, None);
        assert_eq!(off.end_time, None);
    }
}
