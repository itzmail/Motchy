//! Command Code (commandcode.ai, the `cmd` terminal app): the plan's 5-hour
//! and weekly limits, how much of this month's credits are spent, and any
//! prepaid credits, from the same requests Command Code's own `/usage` makes
//! to `api.commandcode.ai`.
//!
//! The key is the one Command Code keeps after `cmd login`: `apiKey` in
//! `~/.commandcode/auth.json`. It does not expire and nothing renews it, so
//! there is no token to protect from a refresh. It goes to
//! `api.commandcode.ai` only, as `Authorization: Bearer`, and never into the
//! log, an error message or a snapshot. A key the server refuses is
//! [`Trouble::Refused`] ("sign in again"). The numbers need the network, so
//! a [`Reach::Local`] refresh stops at [`Trouble::NeedsOnline`].
//!
//! Four requests, in this order (`MAX_FETCHES`): `whoami` (who, and the
//! organisation if there is one), `billing/credits` (the limits and what is
//! left), `billing/subscriptions` (the plan and its period) and `usage/summary`
//! since the period began (what was spent). The last two are best effort:
//! without them the limits still show.
//!
//! Asked as Oyenza Studio: no `User-Agent` of `cmd`'s.

use super::{mask_email, num, reply_json, time, Ctx, Dirs, Money, NetFail, Provider, ProviderId, Reach, Reply, Request, Snapshot, Source, Spend, Step, Trouble, Want, Window, WindowKind};
use serde_json::Value;

const API: &str = "https://api.commandcode.ai";
const FIVE_HOURS: u32 = 5 * 3600;
const WEEK: u32 = 7 * 86_400;
/// Want 0: where `cmd` keeps the key. Want 1: a file it writes on first run,
/// so a computer with `cmd` but no sign-in is [`Trouble::SignedOut`].
const AUTH: usize = 0;
const INSTALLED: usize = 1;
/// The plan ids Command Code sends, with the names it shows. A plan id
/// carries a version after the name (`individual-go-v1`), so the longest
/// prefix wins (`individual-provider` over `individual-pro`).
const PLANS: [(&str, &str); 7] = [
    ("individual-go", "Go"),
    ("individual-goat", "GOAT"),
    ("individual-pro", "Pro"),
    ("individual-provider", "Provider"),
    ("individual-max", "Max"),
    ("individual-ultra", "Ultra"),
    ("teams-pro", "Teams Pro"),
];

pub struct CommandCode;

impl Provider for CommandCode {
    fn id(&self) -> ProviderId {
        ProviderId::CommandCode
    }

    fn wants(&self, dirs: &Dirs) -> Vec<Want> {
        vec![
            Want::File { path: dirs.home_join(&[".commandcode", "auth.json"]), max: 16 * 1024 },
            Want::File { path: dirs.home_join(&[".commandcode", "config.json"]), max: 64 * 1024 },
        ]
    }

    fn next(&self, cx: &Ctx, replies: &[Result<Reply, NetFail>]) -> Step {
        let key = match key(cx) {
            Ok(k) => k,
            Err(t) => return Step::Done(Err(t)),
        };
        if cx.reach == Reach::Local {
            return Step::Done(Err(Trouble::NeedsOnline));
        }
        // Who first: the organisation, if any, goes into the other asks.
        let who = match replies.first() {
            None => return Step::Fetch(ask("/alpha/whoami", &[("limits", "1")], &key)),
            Some(r) => match reply_json(r) {
                Ok(v) => v,
                Err(t) => return Step::Done(Err(t)),
            },
        };
        // The limits are the point: a refused or unreadable answer ends it
        // here, with no more asks.
        if let Some(Err(t)) = replies.get(1).map(reply_json) {
            return Step::Done(Err(t));
        }
        let org = who["org"]["id"].as_str().filter(|o| url_safe(o, |c| c == '-' || c == '_'));
        let org_q: Vec<(&str, &str)> = org.map(|o| ("orgId", o)).into_iter().collect();
        match replies.len() {
            1 => Step::Fetch(ask("/alpha/billing/credits", &org_q, &key)),
            2 => Step::Fetch(ask("/alpha/billing/subscriptions", &org_q, &key)),
            3 => {
                // The summary counts from the start of the billing period.
                let since = replies.get(2).and_then(|r| reply_json(r).ok()).and_then(|s| period(&s, "currentPeriodStart"));
                let mut q = org_q;
                if let Some(s) = since.as_deref() {
                    q.push(("since", s));
                }
                Step::Fetch(ask("/alpha/usage/summary", &q, &key))
            }
            _ => Step::Done(snapshot(cx.now, &who, replies)),
        }
    }

    fn min_gap_s(&self) -> u64 {
        300
    }
}

/// The key to ask with, or why there is none.
fn key(cx: &Ctx) -> Result<String, Trouble> {
    let key = cx.got(AUTH).json().and_then(|v| v["apiKey"].as_str().map(|k| k.trim().to_string())).filter(|k| !k.is_empty());
    match key {
        // Printable ASCII only, so a mangled key never reaches a header
        // (where an HTTP error could quote it back).
        Some(k) if k.bytes().all(|b| b.is_ascii_graphic()) => Ok(k),
        _ if !cx.got(AUTH).is_missing() || !cx.got(INSTALLED).is_missing() => Err(Trouble::SignedOut),
        _ => Err(Trouble::NotFound),
    }
}

fn ask(path: &str, query: &[(&str, &str)], key: &str) -> Request {
    let mut url = format!("{API}{path}");
    for (i, (k, v)) in query.iter().enumerate() {
        url.push(if i == 0 { '?' } else { '&' });
        url.push_str(k);
        url.push('=');
        // A time's colons are the only thing to escape: values are checked.
        url.push_str(&v.replace(':', "%3A"));
    }
    Request::get(url).bearer(key).header("Accept", "application/json")
}

/// Letters, digits and what `extra` allows: a value that goes into an
/// address only if it can't change it.
fn url_safe(s: &str, extra: impl Fn(char) -> bool) -> bool {
    !s.is_empty() && s.len() <= 80 && s.chars().all(|c| c.is_ascii_alphanumeric() || extra(c))
}

/// A period time from the subscription ("2026-10-22T09:31:07.000Z"), only if
/// it looks like one.
fn period(sub: &Value, field: &str) -> Option<String> {
    sub["data"][field].as_str().filter(|t| url_safe(t, |c| matches!(c, '-' | ':' | '.'))).map(String::from)
}

fn plan_name(id: &str) -> Option<&'static str> {
    let id = id.to_ascii_lowercase().replace('_', "-");
    PLANS.iter().filter(|(p, _)| id.starts_with(p)).max_by_key(|(p, _)| p.len()).map(|(_, name)| *name)
}

fn snapshot(now: i64, who: &Value, replies: &[Result<Reply, NetFail>]) -> Result<Snapshot, Trouble> {
    // The limits are the point: without them there is no answer. The plan and
    // the spend are best effort.
    let credits = reply_json(&replies[1])?;
    let sub = reply_json(&replies[2]).ok();
    let summary = reply_json(&replies[3]).ok();
    let mut snap = Snapshot::new(ProviderId::CommandCode, now, Source::Online);
    snap.account = who["user"]["email"].as_str().filter(|e| e.contains('@')).map(mask_email);
    snap.plan = sub.as_ref().and_then(|s| s["data"]["planId"].as_str()).and_then(plan_name).map(String::from);

    // The 5-hour and weekly limits, in dollars of use against their cap.
    for (field, label, kind, length) in [("fiveHour", "5-hour", WindowKind::Session, FIVE_HOURS), ("weekly", "Weekly", WindowKind::Weekly, WEEK)] {
        let w = &credits["windowLimits"][field];
        let Some(cap) = num(&w["cap"]).filter(|c| *c > 0.0) else { continue };
        let used = num(&w["used"]).unwrap_or(0.0).max(0.0);
        // 0 is how it says "not running".
        let resets = time(&w["resetAt"]).filter(|t| *t > 0);
        snap.windows.push(Window::new(kind, label, (used / cap) as f32).resets(resets).length(Some(length)));
    }

    // This month's credits: what is left plus what the period spent is the
    // pool. Only for a plan that is running.
    let spent = summary.as_ref().and_then(|s| num(&s["totalMonthlyCredits"]));
    let left = num(&credits["credits"]["monthlyCredits"]);
    let active = sub.as_ref().is_none_or(|s| s["data"]["status"].as_str().is_none_or(|st| st == "active"));
    if let (Some(spent), Some(left), true) = (spent, left, active) {
        let pool = spent.max(0.0) + left.max(0.0);
        if pool > 0.0 {
            let ends = sub.as_ref().and_then(|s| period(s, "currentPeriodEnd")).and_then(|t| super::parse_time(&t));
            snap.windows.push(Window::new(WindowKind::Monthly, "Credits", (spent.max(0.0) / pool) as f32).resets(ends));
        }
    }
    if let Some(cost) = summary.as_ref().and_then(|s| num(&s["totalCost"])) {
        snap.spend.push(Spend { label: "This period".into(), money: Money::usd(cost), cap: None });
    }
    // Credits bought or given, which don't reset with the month.
    let extra = num(&credits["credits"]["purchasedCredits"]).unwrap_or(0.0).max(0.0) + num(&credits["credits"]["freeCredits"]).unwrap_or(0.0).max(0.0);
    if extra > 0.0 {
        snap.balance = Some(Money::usd(extra));
    }
    if snap.windows.is_empty() && snap.balance.is_none() && snap.spend.is_empty() {
        return Err(Trouble::Unreadable("no limits".into()));
    }
    Ok(snap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FileRead, Got, Os, Outcome, MAX_FETCHES};
    use std::path::PathBuf;

    /// 2026-11-06 (before the fixture's resets).
    const NOW: i64 = 1_794_000_000;
    /// Answers shaped like Command Code's, everything personal replaced.
    const AUTH_FILE: &str = include_str!("fixtures/commandcode/auth.json");
    const WHOAMI: &str = include_str!("fixtures/commandcode/whoami.json");
    const CREDITS: &str = include_str!("fixtures/commandcode/credits.json");
    const SUB: &str = include_str!("fixtures/commandcode/subscription.json");
    const SUMMARY: &str = include_str!("fixtures/commandcode/summary.json");

    fn file(text: &str) -> Got {
        Got::One(FileRead { path: PathBuf::from("file"), bytes: text.as_bytes().to_vec(), modified: None })
    }

    fn got(auth: Option<&str>, installed: bool) -> Vec<Got> {
        vec![auth.map_or(Got::Missing, file), if installed { file("{}") } else { Got::Missing }]
    }

    fn ok(body: &str) -> Result<Reply, NetFail> {
        Ok(Reply { status: 200, body: body.into(), retry_after_s: None })
    }

    fn status(code: u16) -> Result<Reply, NetFail> {
        Ok(Reply { status: code, body: String::new(), retry_after_s: None })
    }

    fn good() -> Vec<Result<Reply, NetFail>> {
        vec![ok(WHOAMI), ok(CREDITS), ok(SUB), ok(SUMMARY)]
    }

    /// One refresh against these answers, in order: what it asked, and the end.
    fn run(got: &[Got], reach: Reach, answers: &[Result<Reply, NetFail>]) -> (Vec<Request>, Outcome) {
        let dirs = Dirs::sample(Os::Mac);
        let cx = Ctx { dirs: &dirs, got, now: NOW, reach };
        let (mut asked, mut replies) = (Vec::new(), Vec::new());
        loop {
            match CommandCode.next(&cx, &replies) {
                Step::Read(w) => panic!("this service never reads in steps: {w:?}"),
                Step::Run(c) => panic!("this service never runs programs: {c:?}"),
                Step::Fetch(req) => {
                    assert!(asked.len() < MAX_FETCHES, "too many asks");
                    let a = answers.get(replies.len()).cloned().unwrap_or_else(|| panic!("no answer for {}", req.url));
                    asked.push(req);
                    replies.push(a);
                }
                Step::Done(out) => return (asked, out),
            }
        }
    }

    fn urls(asked: &[Request]) -> Vec<&str> {
        asked.iter().map(|r| r.url.as_str()).collect()
    }

    #[test]
    fn reads_command_codes_key_file_on_the_mac() {
        assert_eq!(CommandCode.wants(&Dirs::sample(Os::Mac)), vec![
            Want::File { path: PathBuf::from("/Users/u/.commandcode/auth.json"), max: 16 * 1024 },
            Want::File { path: PathBuf::from("/Users/u/.commandcode/config.json"), max: 64 * 1024 },
        ]);
    }

    #[test]
    fn nothing_of_command_code_here_is_not_found() {
        for reach in [Reach::Online, Reach::Local] {
            let (asked, out) = run(&got(None, false), reach, &[]);
            assert!(asked.is_empty());
            assert_eq!(out, Err(Trouble::NotFound));
        }
    }

    #[test]
    fn command_code_without_a_key_is_signed_out() {
        // Ran once, never logged in; a file with no key; a file that isn't JSON; a blank key; a key with a break in it.
        let mangled = AUTH_FILE.replace("user_testkey", "user_\\ntestkey");
        for g in [got(None, true), got(Some("{}"), false), got(Some("{not json"), false), got(Some(r#"{"apiKey":"  "}"#), false), got(Some(&mangled), false)] {
            let (asked, out) = run(&g, Reach::Online, &[]);
            assert!(asked.is_empty(), "no ask without a good key");
            assert_eq!(out, Err(Trouble::SignedOut));
        }
    }

    #[test]
    fn a_local_refresh_asks_nobody() {
        let (asked, out) = run(&got(Some(AUTH_FILE), true), Reach::Local, &[]);
        assert!(asked.is_empty());
        assert_eq!(out, Err(Trouble::NeedsOnline));
    }

    #[test]
    fn the_four_asks_go_to_command_code_with_the_key_and_say_it_all() {
        let (asked, out) = run(&got(Some(AUTH_FILE), true), Reach::Online, &good());
        assert_eq!(urls(&asked), [
            "https://api.commandcode.ai/alpha/whoami?limits=1",
            "https://api.commandcode.ai/alpha/billing/credits",
            "https://api.commandcode.ai/alpha/billing/subscriptions",
            "https://api.commandcode.ai/alpha/usage/summary?since=2026-10-22T09%3A31%3A07.000Z",
        ]);
        for r in &asked {
            assert_eq!(r.headers.iter().find(|(k, _)| k == "Authorization").map(|(_, v)| v.as_str()), Some("Bearer user_testkey0123456789abcdef"));
            assert!(!r.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("user-agent")), "asked as ourselves");
        }
        let s = out.unwrap();
        assert_eq!((s.plan.as_deref(), s.account.as_deref()), (Some("Go"), Some("s•••@example.com")));
        assert_eq!(s.source, Source::Online);
        let by = |kind: WindowKind| s.windows.iter().find(|w| w.kind == kind).unwrap();
        let five = by(WindowKind::Session);
        assert_eq!((five.label.as_str(), five.length_s, five.resets_at), ("5-hour", Some(FIVE_HOURS), Some(1_794_003_600)));
        assert!((five.used - 1.0 / 3.0).abs() < 1e-6);
        let week = by(WindowKind::Weekly);
        assert_eq!((week.used, week.resets_at), (0.25, Some(1_794_203_150)));
        // 2.5 left and 7.5 spent: a pool of 10, three quarters used, until the period ends.
        let month = by(WindowKind::Monthly);
        assert_eq!((month.label.as_str(), month.used, month.resets_at), ("Credits", 0.75, Some(1_795_339_867)));
        assert_eq!(s.spend, [Spend { label: "This period".into(), money: Money::usd(7.5), cap: None }]);
        assert_eq!((s.balance.clone(), s.tokens_today, s.days.len()), (None, None, 0));
        // The key is nowhere in what is kept.
        assert!(!format!("{s:?}").contains("testkey"));
    }

    #[test]
    fn an_organisation_goes_into_the_other_asks() {
        let who = WHOAMI.replace(r#""org":null"#, r#""org":{"id":"org_1-a"}"#);
        let (asked, _) = run(&got(Some(AUTH_FILE), true), Reach::Online, &[ok(&who), ok(CREDITS), ok(SUB), ok(SUMMARY)]);
        assert_eq!(asked[1].url, "https://api.commandcode.ai/alpha/billing/credits?orgId=org_1-a");
        assert!(asked[3].url.starts_with("https://api.commandcode.ai/alpha/usage/summary?orgId=org_1-a&since="));
        // An id that could change the address is no id.
        let who = WHOAMI.replace(r#""org":null"#, r#""org":{"id":"x&y=/z"}"#);
        let (asked, _) = run(&got(Some(AUTH_FILE), true), Reach::Online, &[ok(&who), ok(CREDITS), ok(SUB), ok(SUMMARY)]);
        assert_eq!(asked[1].url, "https://api.commandcode.ai/alpha/billing/credits");
    }

    #[test]
    fn a_refused_key_is_refused_and_a_busy_server_is_busy() {
        let g = got(Some(AUTH_FILE), true);
        assert_eq!(run(&g, Reach::Online, &[status(401)]).1, Err(Trouble::Refused));
        assert_eq!(run(&g, Reach::Online, &[ok(WHOAMI), status(403)]).1, Err(Trouble::Refused));
        assert_eq!(run(&g, Reach::Online, &[ok(WHOAMI), status(429)]).1, Err(Trouble::Busy { retry_after_s: None }));
        assert_eq!(run(&g, Reach::Online, &[Err(NetFail::Timeout)]).1, Err(Trouble::Offline));
        assert!(matches!(run(&g, Reach::Online, &[ok(WHOAMI), ok("<html>")]).1, Err(Trouble::Unreadable(_))));
    }

    #[test]
    fn the_limits_show_even_when_the_plan_and_the_spend_do_not_answer() {
        let (asked, out) = run(&got(Some(AUTH_FILE), true), Reach::Online, &[ok(WHOAMI), ok(CREDITS), status(500), status(500)]);
        assert_eq!(asked.len(), 4);
        assert!(!asked[3].url.contains("since"), "no period known: no start to count from");
        let s = out.unwrap();
        assert_eq!((s.plan, s.windows.len(), s.spend.len()), (None, 2, 0));
        assert!(s.windows.iter().all(|w| w.kind != WindowKind::Monthly), "nothing spent is known: no month bar");
    }

    #[test]
    fn unknown_is_not_zero_and_empty_is_unreadable() {
        let credits = r#"{"credits":{"monthlyCredits":0,"purchasedCredits":0,"freeCredits":0},"windowLimits":{"limited":false,"fiveHour":{"used":0,"cap":0,"resetAt":0}}}"#;
        let (_, out) = run(&got(Some(AUTH_FILE), true), Reach::Online, &[ok(WHOAMI), ok(credits), status(500), status(500)]);
        assert_eq!(out, Err(Trouble::Unreadable("no limits".into())));
        // A window with no cap is no window; a reset of 0 is no reset; prepaid credits show.
        let credits = r#"{"credits":{"monthlyCredits":1,"purchasedCredits":4,"freeCredits":1.5},"windowLimits":{"fiveHour":{"used":2,"cap":4,"resetAt":0},"weekly":{"used":1,"cap":0}}}"#;
        let (_, out) = run(&got(Some(AUTH_FILE), true), Reach::Online, &[ok(WHOAMI), ok(credits), status(500), status(500)]);
        let s = out.unwrap();
        assert_eq!(s.windows.len(), 1);
        assert_eq!((s.windows[0].used, s.windows[0].resets_at), (0.5, None));
        assert_eq!(s.balance, Some(Money::usd(5.5)));
    }

    #[test]
    fn a_plan_that_is_not_running_has_no_month_bar() {
        let sub = SUB.replace(r#""status":"active""#, r#""status":"canceled""#);
        let (_, out) = run(&got(Some(AUTH_FILE), true), Reach::Online, &[ok(WHOAMI), ok(CREDITS), ok(&sub), ok(SUMMARY)]);
        let s = out.unwrap();
        assert!(s.windows.iter().all(|w| w.kind != WindowKind::Monthly));
        assert_eq!(s.plan.as_deref(), Some("Go"));
    }

    #[test]
    fn plans_are_named_by_their_longest_prefix() {
        for (id, name) in [("individual-go-v1", "Go"), ("individual-goat", "GOAT"), ("individual-pro-v1", "Pro"), ("individual_provider", "Provider"), ("individual-max", "Max"), ("individual-ultra-v2", "Ultra"), ("teams-pro", "Teams Pro")] {
            assert_eq!(plan_name(id), Some(name), "{id}");
        }
        assert_eq!(plan_name("free"), None);
        assert_eq!(plan_name(""), None);
    }
}
