//! The Notes page's editor: a real macOS text view (undo, selection, IME,
//! spelling) laid over the island where the page leaves a box for it. The
//! page draws everything else; this view exists only while the Notes page
//! is open and at rest, and is hidden otherwise.
//!
//! It is a subview of the island view, whose y axis points down like the
//! canvas, so a canvas rectangle is its frame as it is.

use crate::events::AppEvent;
use crate::window;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool};
use objc2::{define_class, msg_send, sel, MainThreadMarker, MainThreadOnly, Message};
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAppearanceNameDarkAqua, NSAutoresizingMaskOptions, NSBackgroundColorAttributeName, NSColor, NSEvent, NSEventModifierFlags, NSFont,
    NSBezierPath, NSFontAttributeName, NSFontManager, NSForegroundColorAttributeName, NSKernAttributeName, NSMutableParagraphStyle, NSParagraphStyle, NSParagraphStyleAttributeName, NSResponder, NSScrollView,
    NSScrollerStyle, NSSelectionAffinity, NSText, NSTextInputClient, NSTextView, NSView,
};
use objc2_foundation::{NSArray, NSDictionary, NSObject, NSObjectProtocol, NSPoint, NSRange, NSRect, NSNumber, NSSize, NSString, NSValue};
use poni_core::geom::{Color, Rect};
use poni_core::markdown::{self, Enter, Style};
use std::cell::{Cell, RefCell};

const BODY_PT: f64 = 13.5;

define_class!(
    /// The text view: tells the app when it is clicked (to take the keyboard),
    /// edited, or left with Esc, and answers Cmd+C/V/X/A/Z, which an island
    /// without a menu bar would otherwise ignore.
    #[unsafe(super(NSTextView, NSText, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "PoniNoteView"]
    struct NoteView;

    unsafe impl NSObjectProtocol for NoteView {}

    impl NoteView {
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            window::key_for_editor();
            crate::app::post(AppEvent::NoteFocus(true));
            unsafe {
                let _: () = msg_send![super(self), mouseDown: event];
            }
        }

        #[unsafe(method(didChangeText))]
        fn did_change_text(&self) {
            unsafe {
                let _: () = msg_send![super(self), didChangeText];
            }
            if !self.hasMarkedText() {
                restyle(self);
            }
            crate::app::post(AppEvent::NoteEdited(self.string().to_string()));
        }

        /// The bullets of the list items (their typed `-` is hidden) are drawn here.
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, dirty: NSRect) {
            unsafe {
                let _: () = msg_send![super(self), drawRect: dirty];
            }
            draw_bullets(self, dirty);
        }

        /// Backspace at the start of a heading, quote or list item's text takes
        /// the (hidden) mark off whole: the line is plain text again.
        #[unsafe(method(deleteBackward:))]
        fn delete_backward(&self, sender: Option<&AnyObject>) {
            let sel = self.selectedRange();
            if sel.length == 0 && sel.location > 0 {
                let (start, content) = self.line_at(sel.location);
                let prefix = markdown::line_prefix(&content);
                if prefix > 0 && sel.location == start + prefix {
                    return self.type_in("", Some(NSRange::new(start, prefix)));
                }
            }
            unsafe {
                let _: () = msg_send![super(self), deleteBackward: sender];
            }
        }

        /// Left from where a heading's text starts goes to the line before, not
        /// into the hidden mark.
        #[unsafe(method(moveLeft:))]
        fn move_left(&self, sender: Option<&AnyObject>) {
            let sel = self.selectedRange();
            if sel.length == 0 && sel.location > 0 {
                let (start, content) = self.line_at(sel.location);
                let hidden = markdown::hidden_prefix(&content);
                if hidden > 0 && sel.location == start + hidden {
                    if start > 0 {
                        self.setSelectedRange(NSRange::new(start - 1, 0));
                    }
                    return;
                }
            }
            unsafe {
                let _: () = msg_send![super(self), moveLeft: sender];
            }
        }

        /// The caret never rests inside a hidden mark, and moving it redraws
        /// which `**`, `*` and backticks show (those the caret touches).
        #[unsafe(method(setSelectedRanges:affinity:stillSelecting:))]
        fn set_selected_ranges(&self, ranges: &NSArray<NSValue>, affinity: NSSelectionAffinity, still: Bool) {
            let mut ranges = ranges.retain();
            if ranges.count() == 1 {
                let r = unsafe { ranges.objectAtIndex(0).rangeValue() };
                if r.length == 0 {
                    let (start, content) = self.line_at(r.location);
                    let hidden = markdown::hidden_prefix(&content);
                    if hidden > 0 && r.location < start + hidden {
                        ranges = NSArray::from_retained_slice(&[unsafe { NSValue::valueWithRange(NSRange::new(start + hidden, 0)) }]);
                    }
                }
            }
            unsafe {
                let _: () = msg_send![super(self), setSelectedRanges: &*ranges, affinity: affinity, stillSelecting: still];
            }
            if !still.as_bool() && !self.hasMarkedText() {
                restyle(self);
            }
        }

        /// Enter at the end of a list line starts the next item (or ends the list).
        #[unsafe(method(insertNewline:))]
        fn insert_newline(&self, sender: Option<&AnyObject>) {
            let text = self.string();
            let sel = self.selectedRange();
            if sel.length == 0 {
                let line = text.lineRangeForRange(sel);
                let content = text.substringWithRange(line).to_string();
                let content = content.trim_end_matches('\n');
                // Only at the end of the line: Enter in the middle splits it as usual.
                if sel.location == line.location + content.encode_utf16().count() {
                    match markdown::on_enter(content) {
                        Enter::Next(prefix) => return self.type_in(&format!("\n{prefix}"), None),
                        Enter::End => return self.type_in("", Some(NSRange::new(line.location, content.encode_utf16().count()))),
                        Enter::Plain => {}
                    }
                }
            }
            unsafe {
                let _: () = msg_send![super(self), insertNewline: sender];
            }
        }

        #[unsafe(method(cancelOperation:))]
        fn cancel_operation(&self, _sender: Option<&AnyObject>) {
            crate::app::post(AppEvent::NoteFocus(false));
        }

        #[unsafe(method(performKeyEquivalent:))]
        fn perform_key_equivalent(&self, event: &NSEvent) -> Bool {
            let mine = self.window().and_then(|w| w.firstResponder()).is_some_and(|r| std::ptr::eq(&*r as *const NSResponder as *const u8, self as *const Self as *const u8));
            let flags = event.modifierFlags();
            let only_cmd = flags.contains(NSEventModifierFlags::Command) && !flags.intersects(NSEventModifierFlags::Control | NSEventModifierFlags::Option);
            if !mine || !only_cmd {
                return Bool::NO;
            }
            let key = event.charactersIgnoringModifiers().map(|s| s.to_string()).unwrap_or_default();
            let shift = flags.contains(NSEventModifierFlags::Shift);
            let action = match key.as_str() {
                "c" => Some(sel!(copy:)),
                "v" => Some(sel!(paste:)),
                "x" => Some(sel!(cut:)),
                "a" => Some(sel!(selectAll:)),
                _ => None,
            };
            if let Some(action) = action {
                let _: Bool = unsafe { msg_send![self, tryToPerform: action, with: None::<&AnyObject>] };
                return Bool::YES;
            }
            if key == "z" {
                if let Some(um) = self.undoManager() {
                    if shift {
                        um.redo();
                    } else {
                        um.undo();
                    }
                }
                return Bool::YES;
            }
            Bool::NO
        }
    }
);

impl NoteView {
    /// The line holding UTF-16 offset `at`: where it starts, and its text.
    fn line_at(&self, at: usize) -> (usize, String) {
        let text = self.string();
        let at = at.min(text.length());
        let line = text.lineRangeForRange(NSRange::new(at, 0));
        (line.location, text.substringWithRange(line).to_string().trim_end_matches('\n').to_string())
    }

    /// Type `s` as the person would (undoable), over `range` or the selection.
    fn type_in(&self, s: &str, range: Option<NSRange>) {
        let range = range.unwrap_or_else(|| self.selectedRange());
        let ns = NSString::from_str(s);
        let any: &AnyObject = ns.as_ref();
        unsafe {
            let _: () = msg_send![self, insertText: any, replacementRange: range];
        }
    }
}

thread_local! {
    /// Where the bullets' (hidden) typed marks are in the text, UTF-16 offsets.
    static BULLETS: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
    /// Styling is under way (it must not start itself again).
    static STYLING: Cell<bool> = const { Cell::new(false) };
    /// The accent colour the list marks are drawn in.
    static ACCENT: Cell<[f64; 3]> = const { Cell::new([0.04, 0.52, 1.0]) };
}

fn font(size: f64, bold: bool) -> Retained<NSFont> {
    if bold {
        NSFont::boldSystemFontOfSize(size)
    } else {
        NSFont::systemFontOfSize(size)
    }
}

fn white(a: f64) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 1.0, 1.0, a)
}

fn hanging(first: f64, head: f64) -> Retained<NSMutableParagraphStyle> {
    let p = NSMutableParagraphStyle::new();
    p.setFirstLineHeadIndent(first);
    p.setHeadIndent(head);
    p
}

/// Draw the note's Markdown: headings, list marks, bold, italic, code and
/// quotes get their look, and the typed marks dim. The text is not touched.
fn restyle(tv: &NoteView) {
    if STYLING.with(|s| s.replace(true)) {
        return;
    }
    restyle_now(tv);
    STYLING.with(|s| s.set(false));
}

fn restyle_now(tv: &NoteView) {
    let Some(storage) = (unsafe { tv.textStorage() }) else { return };
    let text = tv.string().to_string();
    let len = text.encode_utf16().count();
    if len == 0 {
        BULLETS.with(|b| b.borrow_mut().clear());
        return;
    }
    let [r, g, b] = ACCENT.with(Cell::get);
    let accent = NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, 1.0);
    let all = NSRange::new(0, len);
    let (fname, color_name, para_name, bg_name, kern_name) =
        unsafe { (NSFontAttributeName, NSForegroundColorAttributeName, NSParagraphStyleAttributeName, NSBackgroundColorAttributeName, NSKernAttributeName) };
    let mut bullets = Vec::new();
    let set = |name: &NSString, v: &AnyObject, range: NSRange| unsafe { storage.addAttribute_value_range(name, v, range) };

    storage.beginEditing();
    let base = font(BODY_PT, false);
    set(fname, base.as_ref(), all);
    set(color_name, white(0.92).as_ref(), all);
    set(para_name, NSParagraphStyle::defaultParagraphStyle().as_ref(), all);
    storage.removeAttribute_range(bg_name, all);
    storage.removeAttribute_range(kern_name, all);
    let fm = NSFontManager::sharedFontManager(tv.mtm());
    let caret = tv.selectedRange();
    let spans = markdown::spans(&text);
    for sp in &spans {
        let range = NSRange::new(sp.start, sp.len);
        match sp.style {
            Style::Heading(level) => {
                let size = heading_pt(level);
                set(fname, font(size, true).as_ref(), range);
                // A heading with only its mark typed keeps its height.
                let para = NSMutableParagraphStyle::new();
                para.setMinimumLineHeight(size * 1.25);
                set(para_name, para.as_ref(), range);
            }
            Style::Marker => {
                // A heading's or quote's mark is never shown: Backspace at the text's
                // start takes it off. The marks of bold, italic and code show while
                // the caret is in (or at the edge of) what they wrap, so they can be deleted.
                let end = sp.start + sp.len;
                let touched = spans.iter().any(|p| {
                    matches!(p.style, Style::Bold | Style::Italic | Style::Code)
                        && p.start <= sp.start
                        && end <= p.start + p.len
                        && caret.location <= p.start + p.len
                        && caret.location + caret.length >= p.start
                });
                if touched {
                    set(color_name, white(0.33).as_ref(), range);
                } else {
                    set(fname, font(0.5, false).as_ref(), range);
                    set(color_name, NSColor::clearColor().as_ref(), range);
                }
            }
            Style::Item => {
                // A bullet line's text starts after the dot (drawn in the hidden mark's
                // place); a numbered line shows its number and hangs in from it.
                let bullet = spans.iter().find(|b| b.style == Style::Bullet && b.start >= sp.start && b.start < sp.start + sp.len);
                let head = bullet.map_or(16.0, |b| BULLET_GAP + (b.start - sp.start) as f64 * 3.7);
                set(para_name, hanging(0.0, head).as_ref(), range);
            }
            Style::Bullet => {
                // The typed `- ` takes no room but the gap the dot sits in.
                let mark = NSRange::new(sp.start, 2.min(len - sp.start));
                set(fname, font(0.5, false).as_ref(), mark);
                set(color_name, NSColor::clearColor().as_ref(), mark);
                let kern = NSNumber::numberWithDouble(BULLET_GAP);
                set(kern_name, kern.as_ref(), NSRange::new(sp.start + 1, 1.min(len - sp.start - 1)));
                bullets.push(sp.start);
            }
            Style::Number => {
                set(color_name, accent.as_ref(), range);
                set(fname, font(BODY_PT, true).as_ref(), range);
            }
            Style::Bold | Style::Italic => {
                // On top of whatever the line already is (a heading stays large).
                let cur = unsafe { storage.attribute_atIndex_effectiveRange(fname, sp.start, std::ptr::null_mut()) }.and_then(|o| o.downcast::<NSFont>().ok()).unwrap_or_else(|| font(BODY_PT, false));
                let mask = if sp.style == Style::Bold { objc2_app_kit::NSFontTraitMask::BoldFontMask } else { objc2_app_kit::NSFontTraitMask::ItalicFontMask };
                set(fname, fm.convertFont_toHaveTrait(&cur, mask).as_ref(), range);
            }
            Style::Code => {
                set(fname, NSFont::monospacedSystemFontOfSize_weight(BODY_PT - 1.0, 0.0).as_ref(), range);
                set(bg_name, white(0.10).as_ref(), range);
            }
            Style::Quote => {
                set(color_name, white(0.58).as_ref(), range);
                set(para_name, hanging(10.0, 10.0).as_ref(), range);
            }
        }
    }
    storage.endEditing();
    BULLETS.with(|b| *b.borrow_mut() = bullets);
    tv.setNeedsDisplay(true);

    // What the person types next looks like its line: large in a heading,
    // never as small as a hidden mark it follows.
    if caret.length == 0 {
        let heading = spans.iter().find_map(|p| match p.style {
            Style::Heading(l) if p.start <= caret.location && caret.location <= p.start + p.len => Some(l),
            _ => None,
        });
        let f = font(heading.map_or(BODY_PT, heading_pt), heading.is_some());
        let color = white(0.92);
        let (k1, k2): (&NSString, &NSString) = unsafe { (NSFontAttributeName, NSForegroundColorAttributeName) };
        let attrs = NSDictionary::from_slices(&[k1, k2], &[f.as_ref() as &AnyObject, color.as_ref() as &AnyObject]);
        unsafe { tv.setTypingAttributes(&attrs) };
    }
}

/// The room, in points, between a bullet's dot and the text after it.
const BULLET_GAP: f64 = 13.0;

/// A dot where each bullet line's typed mark is (it takes no room itself).
fn draw_bullets(tv: &NoteView, dirty: NSRect) {
    let Some(lm) = (unsafe { tv.layoutManager() }) else { return };
    let origin = tv.textContainerOrigin();
    let [r, g, b] = ACCENT.with(Cell::get);
    NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, 1.0).setFill();
    BULLETS.with(|bullets| {
        for &at in bullets.borrow().iter() {
            let glyph = lm.glyphIndexForCharacterAtIndex(at);
            let (used, at_x) = unsafe { (lm.lineFragmentUsedRectForGlyphAtIndex_effectiveRange(glyph, std::ptr::null_mut()), lm.locationForGlyphAtIndex(glyph).x) };
            let (x, y) = (origin.x + used.origin.x + at_x, origin.y + used.origin.y + used.size.height / 2.0);
            let dot = NSRect::new(NSPoint::new(x + 2.0, y - 2.5), NSSize::new(5.0, 5.0));
            if dot.origin.y + 5.0 >= dirty.origin.y && dot.origin.y <= dirty.origin.y + dirty.size.height {
                NSBezierPath::bezierPathWithOvalInRect(dot).fill();
            }
        }
    });
}

/// A heading's size by level.
fn heading_pt(level: u8) -> f64 {
    [22.0, 18.0, 16.0, 15.0, 14.0, 14.0][usize::from(level.clamp(1, 6)) - 1]
}

struct Editor {
    scroll: Retained<NSScrollView>,
    text: Retained<NoteView>,
    /// The note whose text the view holds.
    shown: Option<String>,
}

thread_local! {
    static EDITOR: RefCell<Option<Editor>> = const { RefCell::new(None) };
}

fn rect(r: Rect) -> NSRect {
    NSRect::new(NSPoint::new(r.x as f64, r.y as f64), NSSize::new(r.w as f64, r.h as f64))
}

fn build(mtm: MainThreadMarker, r: Rect, caret: Color) -> Editor {
    let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), rect(r));
    scroll.setDrawsBackground(false);
    scroll.setHasVerticalScroller(true);
    scroll.setHasHorizontalScroller(false);
    scroll.setAutohidesScrollers(true);
    scroll.setScrollerStyle(NSScrollerStyle::Overlay);
    scroll.setBorderType(objc2_app_kit::NSBorderType::NoBorder);
    // The island is dark whatever the Mac's appearance: selection, caret and
    // scroller follow it.
    if let Some(dark) = NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua }) {
        scroll.setAppearance(Some(&dark));
    }

    let size = scroll.contentSize();
    let text: Retained<NoteView> = unsafe { msg_send![NoteView::alloc(mtm), initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), size)] };
    text.setRichText(false);
    text.setImportsGraphics(false);
    text.setAllowsUndo(true);
    text.setAutomaticQuoteSubstitutionEnabled(false);
    text.setAutomaticDashSubstitutionEnabled(false);
    text.setAutomaticTextReplacementEnabled(false);
    text.setAutomaticSpellingCorrectionEnabled(false);
    text.setDrawsBackground(false);
    text.setFont(Some(&NSFont::systemFontOfSize(BODY_PT)));
    text.setTextColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 1.0, 1.0, 0.92)));
    text.setInsertionPointColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(caret.r as f64, caret.g as f64, caret.b as f64, 1.0)));
    text.setTextContainerInset(NSSize::new(6.0, 8.0));
    text.setMinSize(NSSize::new(0.0, size.height));
    text.setMaxSize(NSSize::new(f64::MAX, f64::MAX));
    text.setVerticallyResizable(true);
    text.setHorizontallyResizable(false);
    text.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
    if let Some(tc) = unsafe { text.textContainer() } {
        tc.setContainerSize(NSSize::new(size.width, f64::MAX));
        tc.setWidthTracksTextView(true);
    }
    // Ask for the layout manager now: the bullets' places come from it (TextKit 1).
    let _ = unsafe { text.layoutManager() };
    scroll.setDocumentView(Some(&text));
    Editor { scroll, text, shown: None }
}

/// Put the editor in `r` (canvas DIPs), holding note `id`'s `text`. The text
/// is loaded only when the note changes, so typing is never overwritten.
pub fn show(r: Rect, id: &str, text: &str, caret: Color) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let Some(host) = window::view() else { return };
    ACCENT.with(|a| a.set([caret.r as f64, caret.g as f64, caret.b as f64]));
    EDITOR.with(|e| {
        let mut e = e.borrow_mut();
        let ed = e.get_or_insert_with(|| build(mtm, r, caret));
        if unsafe { ed.scroll.superview() }.is_none() {
            host.addSubview(&ed.scroll);
        }
        ed.scroll.setFrame(rect(r));
        ed.scroll.setHidden(false);
        if ed.shown.as_deref() != Some(id) {
            ed.text.setString(&NSString::from_str(text));
            restyle(&ed.text);
            ed.text.setSelectedRange(NSRange::new(0, 0));
            ed.text.scrollRangeToVisible(NSRange::new(0, 0));
            if let Some(um) = ed.text.undoManager() {
                um.removeAllActions();
            }
            ed.shown = Some(id.to_string());
        }
    });
}

/// Take the editor off the island (the page closed, or the island is moving).
pub fn hide() {
    EDITOR.with(|e| {
        if let Some(ed) = e.borrow_mut().as_mut() {
            if !ed.scroll.isHidden() {
                if ed.text.window().and_then(|w| w.firstResponder()).is_some_and(|r| std::ptr::eq(&*r as *const NSResponder as *const u8, &*ed.text as *const NoteView as *const u8)) {
                    window::first_responder_to_view();
                }
                ed.scroll.setHidden(true);
            }
            // The next time it shows, the note's text is read again.
            ed.shown = None;
        }
    });
}

/// Start typing in the editor (a new note was made).
pub fn focus() {
    window::key_for_editor();
    EDITOR.with(|e| {
        if let Some(ed) = e.borrow().as_ref() {
            if let Some(w) = ed.text.window() {
                let _ = w.makeFirstResponder(Some(&ed.text));
            }
        }
    });
}
