//! `forge logs <target> [-f] [--since] [--grep] [--before]`, `forge logs --list`,
//! `forge daemon logs`, `forge run ... logs`.

use forge_ipc::api::{
    LogLineDto, LogSource, LogTargetDto, LogTargetKind, LogsReadParams, LogsSubscribeParams,
    NoParams,
};
use forge_ipc::client::SubscriptionItem;

use crate::context::Context;
use crate::error::CliResult;
use crate::messages::Msg;
use crate::output::{DIM, Output, RED, or_dash, paint};

/// What to read.
pub struct LogsRequest {
    pub target: String,
    pub follow: bool,
    /// `10m`.
    pub since: Option<String>,
    pub grep: Option<String>,
    pub lines: Option<u32>,
    /// Page back: only lines with a smaller `seq`.
    pub before: Option<u64>,
}

/// Lines shown first when following without `-n`.
const DEFAULT_TAIL: u32 = 50;

fn human_line(line: &LogLineDto) -> String {
    let time = line.ts.map_or_else(
        || "        ".to_owned(),
        |t| t.strftime("%H:%M:%S").to_string(),
    );
    let text = if line.source == LogSource::Stderr {
        paint(RED, &line.text)
    } else {
        line.text.clone()
    };
    format!("{} {text}", paint(DIM, time))
}

pub async fn run(ctx: &Context, request: LogsRequest) -> CliResult<()> {
    let client = ctx.client().await?;
    if request.follow {
        let mut subscription = client
            .logs_subscribe(LogsSubscribeParams {
                target: request.target.clone(),
                grep: request.grep.clone(),
                tail: Some(request.lines.unwrap_or(DEFAULT_TAIL)),
                resume_from: None,
            })
            .await?;
        loop {
            tokio::select! {
                item = subscription.next_item() => match item {
                    Some(SubscriptionItem::Item(envelope)) => {
                        if ctx.out.json {
                            ctx.out.json_line(&envelope.line);
                        } else {
                            ctx.out.line(human_line(&envelope.line));
                        }
                    }
                    Some(SubscriptionItem::Lagged { missed }) => {
                        ctx.out.warn(ctx.out.tf(Msg::LogsLagged, &[&missed]));
                    }
                    None => return Ok(()),
                },
                _ = tokio::signal::ctrl_c() => {
                    subscription.unsubscribe().await;
                    return Ok(());
                }
            }
        }
    }
    let since_secs = match &request.since {
        Some(text) => Some(
            u32::try_from(crate::parse::duration(text)?.as_secs()).map_err(|_| {
                crate::error::CliError::usage(format!("duração grande demais: {text}"))
            })?,
        ),
        None => None,
    };
    let chunk = client
        .logs_read(LogsReadParams {
            target: request.target,
            since_secs,
            grep: request.grep,
            limit: request.lines,
            before_seq: request.before,
        })
        .await?;
    ctx.out.emit(&chunk, |out| {
        if chunk.lines.is_empty() {
            out.line(out.t(Msg::NoLogLines));
        }
        for line in &chunk.lines {
            out.line(human_line(line));
        }
    });
    Ok(())
}

fn kind_name(kind: LogTargetKind) -> &'static str {
    match kind {
        LogTargetKind::Daemon => "daemon",
        LogTargetKind::Instance => "instance",
        LogTargetKind::InstanceFile => "instance_file",
        LogTargetKind::Pool => "pool",
        LogTargetKind::Site => "site",
        LogTargetKind::App => "app",
    }
}

fn targets_table(out: &Output, targets: &[LogTargetDto]) {
    let rows = targets
        .iter()
        .map(|t| {
            vec![
                t.target.clone(),
                kind_name(t.kind).to_owned(),
                t.group.clone(),
                or_dash(t.format.as_deref()),
                if t.live {
                    out.t(Msg::Yes).to_owned()
                } else {
                    out.t(Msg::No).to_owned()
                },
            ]
        })
        .collect();
    out.table(
        &[
            out.t(Msg::HTarget),
            out.t(Msg::HType),
            out.t(Msg::HGroup),
            out.t(Msg::HFormat),
            out.t(Msg::HLive),
        ],
        rows,
        Msg::NoLogTargets,
    );
}

/// `forge logs --list` (`logs.targets`).
pub async fn targets(ctx: &Context) -> CliResult<()> {
    let list = ctx
        .call::<forge_ipc::api::methods::logs_targets>(NoParams {})
        .await?;
    ctx.out.emit(&list, |out| targets_table(out, &list.targets));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::strip_ansi;

    #[test]
    fn lines_show_time_and_text() {
        let line = LogLineDto {
            seq: 1,
            ts: Some("2026-10-27T12:34:56Z".parse().unwrap()),
            source: LogSource::Stdout,
            level: None,
            text: "hello".into(),
            access: None,
        };
        assert_eq!(strip_ansi(&human_line(&line)), "12:34:56 hello");
        let untimed = LogLineDto { ts: None, ..line };
        assert_eq!(strip_ansi(&human_line(&untimed)), "         hello");
    }
}
