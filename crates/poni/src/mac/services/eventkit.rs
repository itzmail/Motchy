//! The Mac's Calendar (EventKit): the events of a day for the Today page, and
//! a new event made from it. Everything runs on the main thread. The
//! `EKEventStore` is made the first time it is needed, and macOS asks the
//! person once (System Settings › Privacy & Security › Calendars changes
//! the answer later).
//!
//! Nothing runs between asks. While the Today page is open, [`watch`] listens
//! to macOS's own "the calendars changed" notification (no polling);
//! [`unwatch`] lets go of it when the page closes.

use crate::events::{AppEvent, EventSink};
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, ProtocolObject};
use objc2_event_kit::{EKAuthorizationStatus, EKCalendar, EKEntityType, EKEvent, EKEventStore, EKEventStoreChangedNotification, EKReminder, EKSpan};
use objc2_foundation::{NSArray, NSDate, NSError, NSNotification, NSNotificationCenter, NSObjectProtocol, NSString};
use poni_core::todo::Remote;
use std::cell::RefCell;
use std::ptr::NonNull;

pub use poni_core::agenda::{Access, Calendar, Event};

thread_local! {
    static STORE: RefCell<Option<Retained<EKEventStore>>> = const { RefCell::new(None) };
    static WATCH: RefCell<Option<Retained<ProtocolObject<dyn NSObjectProtocol>>>> = const { RefCell::new(None) };
    static REMINDERS_WATCH: RefCell<Option<Retained<ProtocolObject<dyn NSObjectProtocol>>>> = const { RefCell::new(None) };
}

/// The Reminders list Tasks is kept the same as.
const LIST: &str = "Motchy";

fn store() -> Retained<EKEventStore> {
    STORE.with(|s| s.borrow_mut().get_or_insert_with(|| unsafe { EKEventStore::new() }).clone())
}

pub fn access() -> Access {
    match unsafe { EKEventStore::authorizationStatusForEntityType(EKEntityType::Event) } {
        EKAuthorizationStatus::NotDetermined => Access::Ask,
        EKAuthorizationStatus::FullAccess => Access::Granted,
        _ => Access::Denied,
    }
}

/// Ask macOS for the calendars; the answer comes back as `AppEvent::Calendar`.
/// A Mac that already answered answers again at once.
pub fn request(sink: EventSink) {
    let done = RcBlock::new(move |granted: Bool, _err: *mut NSError| {
        sink.send(AppEvent::Calendar(if granted.as_bool() { Access::Granted } else { Access::Denied }));
    });
    unsafe { store().requestFullAccessToEventsWithCompletion(RcBlock::as_ptr(&done)) };
}

/// Tell the app when the calendars change (an event added in Calendar.app, a
/// sync that brought one), as `AppEvent::CalendarChanged`. Once per page visit.
pub fn watch(sink: EventSink) {
    if WATCH.with(|w| w.borrow().is_some()) {
        return;
    }
    let changed = RcBlock::new(move |_n: NonNull<NSNotification>| sink.send(AppEvent::CalendarChanged));
    let observer = unsafe { NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(Some(EKEventStoreChangedNotification), None, None, &changed) };
    WATCH.with(|w| *w.borrow_mut() = Some(observer));
}

/// Stop listening (the Today page closed).
pub fn unwatch() {
    if let Some(observer) = WATCH.with(|w| w.borrow_mut().take()) {
        let any: &AnyObject = observer.as_ref();
        unsafe { NSNotificationCenter::defaultCenter().removeObserver(any) };
    }
}

fn date(unix: i64) -> Retained<NSDate> {
    NSDate::dateWithTimeIntervalSince1970(unix as f64)
}

/// The events that touch `[from, to)` (Unix seconds), earliest first.
pub fn events(from: i64, to: i64) -> Vec<Event> {
    if access() != Access::Granted {
        return Vec::new();
    }
    let store = store();
    let found = unsafe {
        let predicate = store.predicateForEventsWithStartDate_endDate_calendars(&date(from), &date(to), None);
        store.eventsMatchingPredicate(&predicate)
    };
    let mut out: Vec<Event> = found
        .iter()
        .map(|e| unsafe {
            let calendar = e.calendar();
            let link = e.URL().and_then(|u| u.absoluteString()).map(|u| u.to_string());
            let (place, notes) = (e.location().map(|s| s.to_string()), e.notes().map(|s| s.to_string()));
            let texts = [link.as_deref().unwrap_or(""), place.as_deref().unwrap_or(""), notes.as_deref().unwrap_or("")];
            Event {
                title: e.title().to_string(),
                start: e.startDate().timeIntervalSince1970() as i64,
                end: e.endDate().timeIntervalSince1970() as i64,
                all_day: e.isAllDay(),
                calendar: calendar.as_ref().map(|c| c.title().to_string()).unwrap_or_default(),
                rgb: calendar.as_ref().and_then(|c| rgb(c)).unwrap_or([0.55, 0.55, 0.6]),
                link: poni_core::agenda::find_link(&texts),
            }
        })
        .collect();
    out.sort_by_key(|e| (!e.all_day, e.start));
    out
}

/// A calendar's colour as sRGB.
fn rgb(c: &EKCalendar) -> Option<[f32; 3]> {
    use objc2_app_kit::{NSColorSpace, NSColor};
    let color: Retained<NSColor> = unsafe { c.color() };
    let c = color.colorUsingColorSpace(&NSColorSpace::sRGBColorSpace())?;
    Some([c.redComponent() as f32, c.greenComponent() as f32, c.blueComponent() as f32])
}

/// The calendars a new event may go into, the Mac's default one first.
pub fn writable_calendars() -> Vec<Calendar> {
    if access() != Access::Granted {
        return Vec::new();
    }
    let store = store();
    let default = unsafe { store.defaultCalendarForNewEvents() }.map(|c| unsafe { c.calendarIdentifier() }.to_string());
    let all: Retained<NSArray<EKCalendar>> = unsafe { store.calendarsForEntityType(EKEntityType::Event) };
    let mut out: Vec<Calendar> = all
        .iter()
        .filter(|c| unsafe { c.allowsContentModifications() })
        .map(|c| unsafe { Calendar { id: c.calendarIdentifier().to_string(), title: c.title().to_string() } })
        .collect();
    out.sort_by_key(|c| default.as_deref() != Some(c.id.as_str()));
    out
}

/// Make an event in the calendar `calendar` (its id; `None` = the Mac's
/// default). The error is one line for the person.
pub fn create_event(title: &str, start: i64, end: i64, all_day: bool, calendar: Option<&str>) -> Result<(), String> {
    if access() != Access::Granted {
        return Err("Calendars isn't allowed for Motchy".into());
    }
    let store = store();
    let target = match calendar {
        Some(id) => unsafe { store.calendarWithIdentifier(&NSString::from_str(id)) },
        None => unsafe { store.defaultCalendarForNewEvents() },
    }
    .ok_or("no calendar to put it in")?;
    unsafe {
        let event = EKEvent::eventWithEventStore(&store);
        event.setTitle(Some(&NSString::from_str(title)));
        event.setStartDate(Some(&date(start)));
        event.setEndDate(Some(&date(end.max(start))));
        event.setAllDay(all_day);
        event.setCalendar(Some(&target));
        store.saveEvent_span_error(&event, EKSpan::ThisEvent).map_err(|e| e.localizedDescription().to_string())
    }
}

// ------------------------------------------------------------------ Reminders

pub fn reminders_access() -> Access {
    match unsafe { EKEventStore::authorizationStatusForEntityType(EKEntityType::Reminder) } {
        EKAuthorizationStatus::NotDetermined => Access::Ask,
        EKAuthorizationStatus::FullAccess => Access::Granted,
        _ => Access::Denied,
    }
}

/// Ask macOS for Reminders; the answer comes back as `AppEvent::Reminders`.
pub fn request_reminders(sink: EventSink) {
    let done = RcBlock::new(move |granted: Bool, _err: *mut NSError| {
        sink.send(AppEvent::Reminders(if granted.as_bool() { Access::Granted } else { Access::Denied }));
    });
    unsafe { store().requestFullAccessToRemindersWithCompletion(RcBlock::as_ptr(&done)) };
}

/// Tell the app when Reminders change, as `AppEvent::RemindersChanged`, until
/// [`unwatch_reminders`]. Only while Tasks sync is switched on.
pub fn watch_reminders(sink: EventSink) {
    if REMINDERS_WATCH.with(|w| w.borrow().is_some()) {
        return;
    }
    let changed = RcBlock::new(move |_n: NonNull<NSNotification>| sink.send(AppEvent::RemindersChanged));
    let observer = unsafe { NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(Some(EKEventStoreChangedNotification), None, None, &changed) };
    REMINDERS_WATCH.with(|w| *w.borrow_mut() = Some(observer));
}

pub fn unwatch_reminders() {
    if let Some(observer) = REMINDERS_WATCH.with(|w| w.borrow_mut().take()) {
        let any: &AnyObject = observer.as_ref();
        unsafe { NSNotificationCenter::defaultCenter().removeObserver(any) };
    }
}

/// The "Motchy" list, made in the account the Mac keeps new reminders in
/// when it isn't there yet.
fn list(store: &EKEventStore) -> Result<Retained<EKCalendar>, String> {
    let all: Retained<NSArray<EKCalendar>> = unsafe { store.calendarsForEntityType(EKEntityType::Reminder) };
    if let Some(found) = all.iter().find(|c| unsafe { c.title().to_string() == LIST && c.allowsContentModifications() }) {
        return Ok(found);
    }
    let source = unsafe { store.defaultCalendarForNewReminders() }.and_then(|c| unsafe { c.source() }).ok_or("Reminders has no account to put a list in")?;
    unsafe {
        let made = EKCalendar::calendarForEntityType_eventStore(EKEntityType::Reminder, store);
        made.setTitle(&NSString::from_str(LIST));
        made.setSource(Some(&source));
        store.saveCalendar_commit_error(&made, true).map_err(|e| e.localizedDescription().to_string())?;
        Ok(made)
    }
}

/// Read the whole list; the answer comes back as `AppEvent::RemindersRead`.
pub fn read_reminders(sink: EventSink) {
    let store = store();
    if reminders_access() != Access::Granted {
        sink.send(AppEvent::RemindersRead(Err("Reminders isn't allowed for Motchy".into())));
        return;
    }
    let calendar = match list(&store) {
        Ok(c) => c,
        Err(e) => return sink.send(AppEvent::RemindersRead(Err(e))),
    };
    let done = RcBlock::new(move |found: *mut NSArray<EKReminder>| {
        // nil means the read failed: an empty list would delete every task.
        let Some(found) = (unsafe { found.as_ref() }) else {
            return sink.send(AppEvent::RemindersRead(Err("Reminders couldn't be read".into())));
        };
        let remote = found
            .iter()
            .map(|r| unsafe {
                Remote {
                    id: r.calendarItemIdentifier().to_string(),
                    text: r.title().to_string(),
                    done: r.isCompleted(),
                    modified: r.lastModifiedDate().map_or(0, |d| d.timeIntervalSince1970() as i64),
                }
            })
            .collect();
        sink.send(AppEvent::RemindersRead(Ok(remote)));
    });
    unsafe {
        let predicate = store.predicateForRemindersInCalendars(Some(&NSArray::from_retained_slice(&[calendar])));
        store.fetchRemindersMatchingPredicate_completion(&predicate, &done);
    }
}

/// Make, change and delete reminders in the list. `create` is (task id,
/// text, done); the reminders made come back as (task id, reminder id). The
/// error is one line for the person.
pub fn write_reminders(create: &[(u64, String, bool)], update: &[(String, String, bool)], delete: &[String]) -> Result<Vec<(u64, String)>, String> {
    let store = store();
    if reminders_access() != Access::Granted {
        return Err("Reminders isn't allowed for Motchy".into());
    }
    let calendar = list(&store)?;
    let err = |e: Retained<NSError>| e.localizedDescription().to_string();
    let find = |id: &str| unsafe { store.calendarItemWithIdentifier(&NSString::from_str(id)) }.and_then(|i| i.downcast::<EKReminder>().ok());
    let mut made = Vec::new();
    unsafe {
        for (id, text, done) in create {
            let r = EKReminder::reminderWithEventStore(&store);
            r.setTitle(Some(&NSString::from_str(text)));
            r.setCalendar(Some(&calendar));
            r.setCompleted(*done);
            store.saveReminder_commit_error(&r, false).map_err(err)?;
            made.push((*id, r));
        }
        for (id, text, done) in update {
            if let Some(r) = find(id) {
                r.setTitle(Some(&NSString::from_str(text)));
                r.setCompleted(*done);
                store.saveReminder_commit_error(&r, false).map_err(err)?;
            }
        }
        for id in delete {
            if let Some(r) = find(id) {
                store.removeReminder_commit_error(&r, false).map_err(err)?;
            }
        }
        store.commit().map_err(err)?;
        Ok(made.into_iter().map(|(id, r)| (id, r.calendarItemIdentifier().to_string())).collect())
    }
}
