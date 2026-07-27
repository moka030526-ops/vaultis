//! The graphical app's built-in manual.
//!
//! This module holds the *content* of the in-app help (a table of topics, each a
//! list of typed blocks) plus the code that renders it as a two-pane browser:
//! a searchable topic index on the left, the selected article on the right.
//!
//! The content is deliberately kept as plain data (`TOPICS`) rather than inline
//! `ui.label(...)` calls so that:
//! - the search box can match against every word of every article without the
//!   article having to be rendered first ([`search`] is a pure function),
//! - a test can assert structural invariants (unique ids, no empty articles),
//! - re-ordering or re-wording the manual never touches rendering code.
//!
//! Nothing here reads or writes the vault: the manual is static text plus the
//! three on-disk paths handed in via [`HelpContext`].
//!
//! Voice: this manual is written for someone who has never used encryption
//! software, and who may be reading it under stress (a new user setting things
//! up for the first time, or an heir opening this cold). Every article says
//! plainly what a thing is, exactly what to click, and what happens next —
//! jargon is explained the first time it appears rather than assumed. The one
//! deliberate exception is the last article in Reference, written for a reader
//! who wants the cryptographic detail behind the plain-language claims made
//! everywhere else.

use eframe::egui;

// --- Content model -----------------------------------------------------------

/// One renderable piece of an article. Keeping these as an `enum` (a closed set
/// of alternatives) means the renderer decides what each kind *looks* like in
/// exactly one place, so the whole manual restyles at once.
#[derive(Debug)]
pub(crate) enum Block {
    /// A paragraph of prose.
    P(&'static str),
    /// A sub-heading inside an article.
    Sub(&'static str),
    /// An unordered list.
    Bullets(&'static [&'static str]),
    /// A numbered list — used for "do this, then this" procedures.
    Steps(&'static [&'static str]),
    /// A two-column reference table (`what` → `what it does`).
    Rows(&'static [(&'static str, &'static str)]),
    /// An aside worth noticing but not alarming.
    Note(&'static str),
    /// A consequence the user cannot undo — rendered in the warning color.
    Warn(&'static str),
}

/// One article in the manual.
pub(crate) struct Topic {
    /// Stable identifier (used by tests and by cross-references; never shown).
    pub(crate) id: &'static str,
    /// The index group this topic is listed under.
    pub(crate) section: &'static str,
    /// The article's title (shown in the index and as its heading).
    pub(crate) title: &'static str,
    /// One line shown under the heading, and matched by the search box.
    pub(crate) blurb: &'static str,
    pub(crate) body: &'static [Block],
}

/// The index groups, in the order they appear in the navigation pane.
pub(crate) const SECTIONS: &[&str] =
    &["Getting started", "The tabs", "Working with records", "Settings & maintenance", "Reference"];

// The manual itself. Written to be read by someone who has never seen the app —
// including an heir opening it read-only for the first time — so each article
// says what the thing is, what it does, and what it cannot do.
pub(crate) const TOPICS: &[Topic] = &[
    // --- Getting started -----------------------------------------------------
    Topic {
        id: "overview",
        section: "Getting started",
        title: "What vaultis is",
        blurb: "One locked folder on your own computer, holding everything an executor or heir will need. Never connected to the internet.",
        body: &[
            Block::P(
                "vaultis stores the information an executor or heir would need — account logins, \
                 the will, property deeds, tax returns, instructions for who to call — in a \
                 single encrypted vault, protected by two passwords. Instead of this information \
                 being spread across sticky notes, old emails, and files only you understand, it \
                 is kept in one place.",
            ),
            Block::P(
                "\"Encrypted\" means the information is scrambled into unreadable gibberish \
                 whenever the vault is closed. Nobody — not someone who steals your laptop, not \
                 someone who copies the files, not the people who wrote this program — can make \
                 sense of it without the two passwords you choose. There is more detail on \
                 exactly how in “How your data is protected”, and a full technical account in \
                 “For the technically curious: why this is safe”.",
            ),
            Block::Sub("Why it is completely offline"),
            Block::P(
                "There is no network code anywhere in this program — not a hidden setting, a \
                 missing feature. The people who built it simply never wrote the code that would \
                 let it connect to the internet, so there is nothing to switch off and nothing \
                 that could be turned on by a future update. Concretely, this means: nothing you \
                 type is ever uploaded anywhere, the app cannot \"phone home\" to check for \
                 updates or report usage, it cannot sync between devices on its own, and it \
                 cannot be reached or tampered with by anyone over a network, because it is never \
                 listening on one. The only way your information leaves this computer is if you \
                 export it yourself.",
            ),
            Block::Sub("What a vault looks like on your computer"),
            Block::P(
                "A vault is not a single file you can double-click, the way a photo or a Word \
                 document is. It is a folder — think of it as a locked drawer — that holds three \
                 things inside it: the record file (vault.pmv, a small file holding all your text \
                 entries — logins, notes, the asset list, and so on), an encrypted index of your \
                 uploaded documents (the manifest/ folder, like a table of contents), and the \
                 documents themselves (the volume/ folder, the actual scanned deeds, PDFs, and \
                 photos you attached). These three parts only make sense together: back up the \
                 WHOLE folder, never just one piece of it, or you will end up with, say, a list \
                 of documents and no documents.",
            ),
            Block::Sub("You can have more than one vault"),
            Block::P(
                "Nothing limits you to a single vault. Many people keep separate vaults for \
                 separate purposes — one for themselves, a second one they are helping a parent \
                 set up, a third for a small business — each completely private from the others, \
                 each with its own two passwords. See “Opening or creating a vault” for exactly \
                 how that works and what to click.",
            ),
            Block::Sub("Three ways to open the same vault"),
            Block::P(
                "This window is the point-and-click version (vaultis-gui), used for everyday \
                 viewing and editing. The same vault can also be opened with a text-based, \
                 keyboard-driven interface (vaultis --tui), and there are separate command-line \
                 tools for bulk jobs like backups and exports. Most people only ever need this \
                 window; the others are covered in “Command line” under Reference.",
            ),
        ],
    },
    Topic {
        id: "opening",
        section: "Getting started",
        title: "Opening or creating a vault",
        blurb: "A plain-English, click-by-click walkthrough of the very first screen: opening a vault you already made, starting a brand-new one, and why you can keep more than one.",
        body: &[
            Block::P(
                "This is the very first screen vaultis shows you, before it will let you see any \
                 of your information. Its only job is to answer one question: which vault do you \
                 want, and what are its two passwords? Nothing on this screen is your data yet — \
                 it is just how you get to it.",
            ),
            Block::Sub("The Vaults folder and the vault inside it"),
            Block::P(
                "The \"Vaults folder\" is an ordinary folder on your computer that you choose. \
                 Each vault inside it is its own folder with its own two passwords, holding its \
                 own records — you can keep more than one, for example one for yourself and \
                 another you are helping a parent set up, and none of them can see what is inside \
                 another. Creating a second vault does not touch the first one.",
            ),
            Block::Sub("Step 1 — choose the Vaults folder"),
            Block::P(
                "The top box, \"Vaults folder\", is where you tell vaultis which folder to look \
                 in. Type or paste a folder path into it — pasting straight from your file \
                 manager works even if the path has quotation marks around it, see “Typing file \
                 and folder paths”. vaultis then looks one level inside that folder for anything \
                 that looks like a vault (technically: any subfolder that contains a vault.pmv \
                 file) and lists what it finds in the dropdown below. vaultis does NOT remember \
                 this folder between launches — it writes nothing outside the folder you point it \
                 at. To skip typing it, start the program from inside your vaults folder, or pass \
                 the folder on the command line.",
            ),
            Block::Sub("Step 2 — pick a vault, or name a new one"),
            Block::P(
                "The second box, \"Vault\", is the vault's own folder name inside the Vaults \
                 folder. If you already have a vault, click the dropdown and choose its name from \
                 the list; that is opening one you already made. If you want to start completely \
                 fresh, type a name that is not already in the list — something like MyVault or \
                 Mom2026 — and vaultis will build a brand-new, empty vault with that name the \
                 moment you finish the next two steps.",
            ),
            Block::Sub("Step 3 — type the two passwords"),
            Block::P(
                "Every vault, new or existing, is locked with two separate passwords typed one \
                 after another. The order you type them in matters — see “The two passwords” for \
                 why — and for a brand-new vault you will be asked to type each one twice, once to \
                 set it and once to confirm you typed what you meant to, because there is no way \
                 to recover a mistyped password later.",
            ),
            Block::Sub("The button tells you which one is about to happen"),
            Block::P(
                "One button does both jobs, and its label changes so you always know which thing \
                 is about to happen before you click:",
            ),
            Block::Rows(&[
                (
                    "Button says Unlock",
                    "vaultis found an existing vault under the name you picked. Clicking Unlock \
                     OPENS it with the two passwords you typed — nothing is created, nothing is \
                     changed, you are simply looking at what is already there. This is what you \
                     will click almost every time you come back to look at or edit something you \
                     already saved.",
                ),
                (
                    "Button says Create",
                    "The name you typed does not exist yet, or the folder is empty. Clicking \
                     Create BUILDS a brand-new, empty vault with that name and those two \
                     passwords, then opens it. This is the one-time step you do only when \
                     starting something new — after that, the exact same vault always reopens \
                     with Unlock.",
                ),
            ]),
            Block::Note(
                "The button says Unlock or Create based only on whether a vault already exists at \
                 the name you picked — it is the same in a read-only session as in a write \
                 session. A read-only session will still show the Create screen for an empty \
                 folder, but clicking Create there is refused with an error telling you to \
                 relaunch with --write, since a read-only session cannot write a new vault to \
                 disk. See “Read-only vs. write mode”.",
            ),
            Block::Sub("Why creating asks for every password twice"),
            Block::P(
                "There is no \"forgot password\" link anywhere in this program, on purpose — see \
                 “The two passwords” for why. A typo you do not catch while creating a vault would \
                 mean permanently losing access to everything you later put in it. Asking for each \
                 password twice, side by side, when the vault is new is the one safety net vaultis \
                 can offer against that: if the two boxes for a password do not match, it tells you \
                 immediately, before anything is written to disk, rather than weeks later when you \
                 have forgotten which version you typed.",
            ),
            Block::Sub("One window per vault"),
            Block::P(
                "If you try to open a vault that is already open in another window, vaultis does \
                 not open a second copy — it simply brings the existing window to the front, as if \
                 you had clicked back to it yourself. And if that existing session is in write \
                 mode, a second attempt to open the same vault for writing is refused outright \
                 rather than allowed to proceed, because two windows both saving changes to the \
                 same vault at the same time could overwrite each other's work.",
            ),
        ],
    },
    Topic {
        id: "passwords",
        section: "Getting started",
        title: "The two passwords",
        blurb: "Why there are two, why the order matters, and why nobody — including the people who wrote this program — can ever recover a lost one.",
        body: &[
            Block::P(
                "Every vault is locked with two separate passwords, typed one after the other, \
                 instead of the single password you might expect from other programs. Neither \
                 password alone opens the vault — both are required, similar to a safe-deposit \
                 box that needs two keys turned together.",
            ),
            Block::P(
                "This is meant to let the two passwords be kept apart from each other on purpose \
                 — for example, you might remember one yourself while a lawyer, a trusted \
                 relative, or a safe holds the other — so that no single place, and no single \
                 person other than you, ever holds everything needed to open the vault.",
            ),
            Block::P(
                "The ORDER you type them in is not interchangeable. Behind the scenes vaultis \
                 combines the two passwords into the vault's real encryption key through a \
                 deliberately slow scrambling process (a technique called Argon2id — explained \
                 fully in “For the technically curious: why this is safe”), where the result of \
                 scrambling the first password feeds into the process for the second. Typing them \
                 in the opposite order does not \"also work\" — it is simply a different, wrong \
                 pair, and the vault will refuse it exactly like any other incorrect guess.",
            ),
            Block::Warn(
                "There is NO \"forgot password\" option, no reset link, no support line that can \
                 help, and no secret backdoor built in for emergencies. If either password is \
                 lost, the data inside is gone for good — this is deliberate, not an oversight, \
                 because any way to recover a forgotten password would also be a way for someone \
                 else to break in without one. Write both passwords down, on paper or in a \
                 password manager you trust, and store them somewhere your executor will actually \
                 find when the time comes — a password only you know is of no use to the people \
                 you are leaving this vault for.",
            ),
            Block::Sub("Changing the two passwords later"),
            Block::Steps(&[
                "Click 🔑 Passwords in the top bar (only shown in write mode).",
                "Type the CURRENT pair of passwords first, to prove you are allowed to change them.",
                "Type each NEW password twice — once to set it, once to confirm there is no typo.",
                "Click the button to confirm. The vault is re-scrambled with a brand-new key; the old passwords stop working the instant this finishes.",
            ]),
            Block::Note(
                "Changing your passwords re-encrypts the WHOLE vault under the new pair — every \
                 document you have already uploaded, not just the small record file — so the old \
                 passwords cannot decrypt anything afterwards, including a copy of the old files \
                 left behind on disk. On a vault with a lot of documents this can take noticeably \
                 longer than an ordinary save; the process is crash-safe, so an interruption \
                 partway through always leaves you with either the fully old vault or the fully \
                 new one, never a mixture.",
            ),
            Block::Sub("Why a wrong password looks exactly like a damaged vault"),
            Block::P(
                "If you type the wrong passwords, or if the vault file has somehow been damaged \
                 or tampered with, vaultis shows you the identical error message either way — it \
                 will not tell you which one happened. That is not a missing feature; it is \
                 deliberate. If vaultis told an attacker \"that password was close, but not quite \
                 right\" versus \"that password was completely wrong\", it would be handing them a \
                 way to slowly narrow down your password by trial and error from outside — the \
                 same trick a thief uses on a bike-lock combination by feeling for the click. \
                 Showing one plain, unhelpful error for both cases denies them that feedback \
                 entirely.",
            ),
        ],
    },
    Topic {
        id: "readonly",
        section: "Getting started",
        title: "Read-only vs. write mode",
        blurb: "Why the app opens looking rather than editing by default, and the exact, complete list of what each mode does and does not let you do.",
        body: &[
            Block::P(
                "vaultis opens READ-ONLY by default — it can look at your vault but cannot \
                 change anything in it — unless it is started with an extra flag (--write). A \
                 read-only session cannot write to the vault at all, so it cannot break anything, \
                 which is why it is the default, and the mode to use if you are not sure you need \
                 to change something.",
            ),
            Block::P(
                "Whenever the app is in this mode, an orange 🔒 READ-ONLY badge is shown in the \
                 top bar the whole time, and every button or box that would change something is \
                 simply not drawn — there is nothing tempting to click by mistake.",
            ),
            Block::Sub("What you CAN still do in read-only mode"),
            Block::Bullets(&[
                "Read every record on every tab. The form still shows all the text, but it \
                 cannot be typed into — it can, however, still be selected and copied with \
                 Ctrl+C, exactly like a normal read-only document.",
                "Reveal and copy passwords, for logging into an account you need.",
                "Export attached documents (deeds, PDFs, scans) to a folder on this computer.",
                "Export an entire tab to a spreadsheet-friendly CSV file. Be aware this file is \
                 written in PLAIN TEXT with no encryption at all, and on the Accounts or Real \
                 Estate tabs it contains every password spelled out in the clear — handle that \
                 exported file with exactly the same care you would the passwords themselves.",
                "Make a backup copy of the still-encrypted vault.",
                "Change the color theme, the interface size, the typeface, and which tabs open \
                 grouped — these are saved in a small prefs.conf in your vaults folder, not inside \
                 the encrypted vault, so changing them does not need write access.",
                "Set where exports go. This one is remembered only for the current session: you \
                 set it again next time you open the app.",
            ]),
            Block::Sub("What you CANNOT do — write mode is required"),
            Block::Bullets(&[
                "Create, edit, or delete any record.",
                "Attach a new document to a record, or remove one that is already attached.",
                "Change the vault's two passwords.",
                "Edit the dropdown lists of types and subtypes, the document-volume size, or the redundancy setting.",
                "Pull in newer records from a second copy of this vault (\"Update from another vault\").",
            ]),
            Block::Note(
                "To switch from read-only to write mode, close this window and start the program \
                 again with the --write flag added. There is no in-app button to switch — closing \
                 and relaunching is the only way, which is itself a small safeguard against \
                 flipping into write mode by accident.",
            ),
        ],
    },
    Topic {
        id: "window",
        section: "Getting started",
        title: "Finding your way around",
        blurb: "A guided tour of every button and area on screen: the top bar, the row of tabs, the strip along the bottom, and the red banner that appears when something goes wrong.",
        body: &[
            Block::Sub("The top bar"),
            Block::P(
                "The very top of the window has two rows. The first row names the vault that is \
                 open and holds the handful of actions that apply everywhere, no matter which tab \
                 you are on. The second row is the strip of tabs — one tab per kind of record, \
                 like folders in a filing cabinet drawer.",
            ),
            Block::Rows(&[
                (
                    "🗄 vault name",
                    "Shows which vault is currently open. If you keep more than one vault, hover \
                     your mouse over this to see the FULL folder path — handy, because two \
                     windows onto two different vaults can otherwise look identical at a glance.",
                ),
                (
                    "WRITE / 🔒 READ-ONLY",
                    "Tells you the session's mode at a glance. The orange read-only badge is your \
                     reminder for why the editing controls you might expect are simply not there \
                     — see “Read-only vs. write mode”.",
                ),
                ("🔑 Passwords", "Opens the screen to change the vault's two passwords (write mode only)."),
                (
                    "⚙ Config",
                    "Opens Settings: appearance/color theme, what each tab looks like by default, \
                     the dropdown lists of types, where exports are written, backups, and storage.",
                ),
                ("❓ Help", "Opens this manual — the screen you are reading right now."),
                (
                    "Quit",
                    "Closes the window. Every secret held in memory is overwritten before the app \
                     exits, and the clipboard is cleared too, so nothing sensitive lingers after \
                     you leave.",
                ),
            ]),
            Block::Sub("What is inside a tab"),
            Block::P(
                "Every record tab is split down the middle into two halves: the LIST of records \
                 on the left, and the FORM for whichever one is selected on the right. Click a row \
                 in the list to load it into the form on the right; click ➕ New to start a blank \
                 one instead. Nothing you type is actually stored anywhere until you click 💾 \
                 Save — up until then, it only exists on your screen.",
            ),
            Block::Note(
                "Typed something and then clicked away without saving it? vaultis now warns you \
                 about that live, right in the strip along the very bottom of the window, the \
                 moment it happens — look for “⚠ unsaved changes” there whenever you have edits \
                 that have not been saved yet. See “Creating, editing, and deleting” for the full \
                 save/discard behaviour.",
            ),
            Block::Sub("The thin status line, and the red banner"),
            Block::P(
                "A thin strip along the very bottom of the window reports the result of whatever \
                 you just did — “Saved.”, “Exported to …”, and so on — and it is also where the \
                 unsaved-changes and clipboard-timer notices described above appear. A genuine \
                 FAILURE — a save that did not actually land, an upload that could not complete — \
                 additionally raises a red banner all the way across the TOP of the window, so a \
                 failure can never be missed just because your eyes happened to be somewhere else \
                 on screen. Dismiss the banner with its button, or simply do something that \
                 succeeds and it clears on its own.",
            ),
        ],
    },
    // --- The tabs ------------------------------------------------------------
    Topic {
        id: "tab-urgent",
        section: "The tabs",
        title: "URGENT",
        blurb: "The handful of things someone must read within hours of opening this vault. Deliberately the very first, leftmost tab.",
        body: &[
            Block::P(
                "URGENT holds the small number of things a person needs to know immediately, not \
                 eventually — where the will physically is, who to phone first, which bill is on \
                 autopay and needs to be stopped, the combination to a safe. Nothing that can \
                 wait a week belongs here.",
            ),
            Block::P(
                "Each entry is simply a title and a free-form block of text — there are no \
                 categories to pick and no required structure to follow, so you can write it the \
                 way you would explain it out loud to someone standing in front of you.",
            ),
            Block::Note(
                "URGENT is deliberately the FIRST tab, on the far left, so it is the first thing \
                 anyone opening this vault under stress sees. Keep it genuinely short — anything \
                 that is important but not time-critical belongs on the Instructions tab instead."
            ),
        ],
    },
    Topic {
        id: "tab-instructions",
        section: "The tabs",
        title: "Instructions",
        blurb: "The longer written guidance that does not fit on a single URGENT note: what to do, in what order, and who to call.",
        body: &[
            Block::P(
                "Instructions is where your fuller written wishes and step-by-step guidance live \
                 — funeral arrangements, how to go about closing out each account, which \
                 professionals (accountant, lawyer, financial advisor) to contact and why, what \
                 you would like done with the house, or a private note for a specific person.",
            ),
            Block::P("Each instruction has a title, a body of text you write freely, and can have documents attached to it, just like the other tabs."),
            Block::Note(
                "Instructions are your own words, not a legal document. Nothing written here \
                 replaces an actual will or trust — use this tab to explain your intentions and \
                 point the reader at the real documents, which belong on the Trust and Will tab.",
            ),
        ],
    },
    Topic {
        id: "tab-trustwill",
        section: "The tabs",
        title: "Trust and Will",
        blurb: "Your estate's legal instruments, and scans of the signed paper originals — plus, importantly, where those originals physically are.",
        body: &[
            Block::P(
                "This tab is for your estate's actual legal instruments: the will itself, any \
                 trusts, powers of attorney, healthcare directives, plus who drew them up and — \
                 crucially — where the signed paper originals are physically kept.",
            ),
            Block::P(
                "Use the Documents section of the form to attach a scan of each one. The copy \
                 kept in the vault is a convenience, so the contents are never lost — but write \
                 down where the SIGNED original lives, because in most cases that paper original, \
                 not the scan, is the one that actually carries legal force.",
            ),
        ],
    },
    Topic {
        id: "tab-assets",
        section: "The tabs",
        title: "Assets and Liabilities",
        blurb: "Everything owned, and everything owed — with an optional link to the login that manages each one.",
        body: &[
            Block::P(
                "One entry per thing you own or owe: real estate, brokerage and retirement \
                 accounts, vehicles, valuables, loans, mortgages, and lines of credit. Each entry \
                 records who owns it, what kind of thing it is, roughly what it is worth, and any \
                 free-form notes you want to keep with it.",
            ),
            Block::Sub("Viewing it as a tree instead of a flat list"),
            Block::P(
                "Tick the \"grouped\" checkbox to switch the list from one long flat list into a \
                 tree — owner, then asset or liability, then type — which keeps a large estate \
                 navigable instead of an endless scroll. Each branch of the tree expands and \
                 collapses on its own, and vaultis remembers which branches you had open.",
            ),
            Block::Sub("Linking to the account that manages it"),
            Block::P(
                "An asset can be tied to the login that holds or services it — for example, the \
                 brokerage login behind a portfolio, or the mortgage servicer's login behind a \
                 house. Links are added from the asset's side of the connection; the matching \
                 Account's form shows the reverse view (\"linked from\") with a button that jumps \
                 straight back. Full walkthrough at “Linking assets and accounts”.",
            ),
            Block::Sub("Narrowing to what needs another look"),
            Block::P(
                "Tick \"review only\" to shrink the list down to just the entries you have flagged \
                 as needing a second pass — useful when you are working your way through a \
                 backlog of half-finished entries a little at a time.",
            ),
        ],
    },
    Topic {
        id: "tab-accounts",
        section: "The tabs",
        title: "Accounts",
        blurb: "Every login and password you keep here, with filters, a forgiving search box, and an optional grouped view.",
        body: &[
            Block::P(
                "Accounts is the tab for logins: bank and brokerage, utilities, insurance, email, \
                 subscriptions, and anything else that has a username and a password. Every \
                 account needs at minimum a Title and an Owner — everything else on the form is \
                 optional, fill in only what you have.",
            ),
            Block::Sub("Narrowing a long list with the filter row"),
            Block::P(
                "The row of filters above the list narrows it by type, subtype, owner, and title. \
                 These filters are smart about each other: each dropdown only ever offers choices \
                 that actually exist among the accounts still matching your OTHER active filters, \
                 so you can never end up staring at a filter combination that produces an \
                 inexplicably empty list. If a filter you had picked stops making sense given the \
                 others, it quietly clears itself rather than leaving you confused.",
            ),
            Block::Rows(&[
                ("type / subtype / owner / title", "Dropdowns that narrow the list; leaving one blank means \"don't filter by this\"."),
                ("review only", "Shows only the accounts you have flagged as needing another look."),
                ("reveal all", "Unmasks every password on this screen at once. See “Passwords: reveal, generate, copy”."),
                ("grouped", "Switches between one flat list and a tree organised owner, then type, then subtype, then title."),
                (
                    "🔍 search",
                    "The outlined, pill-shaped box in the filter row — you can tell it apart from \
                     the plain dropdowns next to it. Type into it and the list narrows as you go, \
                     with no button to press. It matches free text against the username OR the \
                     title, ignores upper/lower case, matches anywhere in the value (not only the \
                     start), and is forgiving of spelling. See “Searching” for exactly how.",
                ),
                ("Clear", "Resets every filter, the review flag, and the search box at once, back to a clean slate."),
                (
                    "Trim all fields",
                    "A one-off cleanup button: it strips stray leading and trailing spaces from \
                     every field of every record in the WHOLE vault, not just this tab (write \
                     mode only; the change is recorded in each record's history).",
                ),
            ]),
            Block::Note(
                "Starting a brand-new account while a filter is active pre-fills the blank form \
                 with those same filter values — so if you filter to a specific type and owner \
                 first, the new record you create already starts half filled-in for you.",
            ),
        ],
    },
    Topic {
        id: "tab-realestate",
        section: "The tabs",
        title: "Real Estate",
        blurb: "One record per property, each carrying four separate portal logins — the four accounts anyone settling an estate always ends up needing.",
        body: &[
            Block::P(
                "One record per property you own: its address, who owns it, purchase and \
                 valuation details, and whatever notes matter. Documents that belong to the \
                 property — deeds, surveys, insurance policies, closing statements — attach \
                 directly to it.",
            ),
            Block::Sub("Why four separate logins"),
            Block::P(
                "Every property record carries four SEPARATE logins, because these are the four \
                 accounts that someone settling an estate almost always ends up needing to find:",
            ),
            Block::Bullets(&[
                "Property management",
                "Insurance",
                "HOA (homeowners' association)",
                "Tax (the property-tax authority or its payment portal)",
            ]),
            Block::P(
                "Each of the four has its own web address, username, password, and a free-form \
                 comment box for the odd details that never fit a labelled field — a security \
                 question's answer, an account number, or who specifically to ask for when you \
                 call.",
            ),
            Block::Note(
                "Real Estate has its own \"reveal all\" switch, kept completely separate from the \
                 one on the Accounts tab, so unmasking passwords on one screen never accidentally \
                 unmasks them on the other.",
            ),
        ],
    },
    Topic {
        id: "tab-taxes",
        section: "The tabs",
        title: "Taxes",
        blurb: "One entry per year's tax filing, each able to hold several attached documents at once.",
        body: &[
            Block::P(
                "One record per filing: which year it covers, which jurisdiction (federal, state, \
                 etc.), who prepared it, and any notes about what was filed. Returns, schedules, \
                 K-1s, and any correspondence with the tax authority attach to the specific year \
                 they belong to.",
            ),
            Block::P(
                "Unlike most other tabs, a single tax filing holds a numbered LIST of several \
                 documents rather than just one — the export and remove buttons act on whichever \
                 entry you pick from that list.",
            ),
            Block::Note("A few years of past returns are usually the very first thing an estate's accountant will ask for."),
        ],
    },
    Topic {
        id: "tab-general",
        section: "The tabs",
        title: "General Documents",
        blurb: "The catch-all tab: anything worth keeping safe that does not belong to an account, a property, or a tax year.",
        body: &[
            Block::P(
                "Passports, birth and marriage certificates, military records, diplomas, \
                 warranties, vehicle titles, membership records — anything that ought to survive \
                 along with the rest of the estate, but has no natural home on any of the other \
                 tabs.",
            ),
            Block::P("Each entry is simply a title, a short description, and its one attached file."),
        ],
    },
    Topic {
        id: "tab-summary",
        section: "The tabs",
        title: "Summary",
        blurb: "A calculated, read-only overview: what each owner has, and what they owe, added up automatically.",
        body: &[
            Block::P(
                "Summary is not a record type you fill in yourself — nothing on this tab can be \
                 typed into or edited. It is calculated automatically from the Assets and \
                 Liabilities tab, totalled per owner, with assets split into buckets (real \
                 estate, cash, before-tax, after-tax) set against a single liability column.",
            ),
            Block::Sub("How vaultis decides which bucket an asset lands in"),
            Block::P(
                "You never have to pick a bucket yourself — vaultis reads the words you already \
                 typed into the record's TYPE and INSTITUTION fields and infers it:",
            ),
            Block::Rows(&[
                ("Real estate", "The type or institution mentions real estate, property, or rental (assets only — never liabilities)."),
                ("Before tax", "Mentions retirement, pension, annuity, IRA, Roth, 401/403/457, TSP, or HSA."),
                ("Cash", "Mentions cash, savings, checking, or money market."),
                ("After tax", "Everything else — an ordinary brokerage account, a vehicle, a valuable item."),
            ]),
        ],
    },
    // --- Working with records ------------------------------------------------
    Topic {
        id: "editing",
        section: "Working with records",
        title: "Creating, editing, and deleting",
        blurb: "The same three-step save cycle on every single tab: click, fill in, save — plus exactly what happens if you forget the last step.",
        body: &[
            Block::Steps(&[
                "Click ➕ New for a blank record, or click an existing row in the list to load it into the form.",
                "Fill in the fields. Dropdown fields are drawn from the type lists set up in Config; a record's existing value stays selectable there even if it has since been removed from that list.",
                "Click 💾 Save. The vault file is rewritten on disk, and the change is added to that record's own history log.",
            ]),
            Block::Warn(
                "Nothing you type is stored anywhere until you click Save. Selecting a different \
                 record, switching to another tab, or closing the app all discard whatever you \
                 were typing, with no confirmation prompt. vaultis now warns you about this LIVE \
                 — look for “⚠ unsaved changes” in the strip along the bottom of the window \
                 whenever you have edits sitting unsaved on screen.",
            ),
            Block::Sub("What a record is required to have"),
            Block::P(
                "Every record needs at least a Title, and an Account additionally needs an Owner — \
                 typing only blank space does not count as filling either in. If you try to save \
                 something missing one of these, vaultis tells you exactly which field it still \
                 needs, rather than silently saving a record nobody could ever find again.",
            ),
            Block::Note(
                "Starting a new record while a filter or a search is active pre-fills the blank \
                 form from them, and saving automatically relaxes any filter that would otherwise \
                 have hidden the record you just created — so a brand-new entry never seems to \
                 vanish the instant you save it.",
            ),
            Block::Sub("Trim all fields"),
            Block::P(
                "A one-off cleanup button, on the Accounts tab, available in write mode: it strips \
                 stray leading and trailing spaces from every field of every record in the WHOLE \
                 vault, not only this tab, and records the change in each record's history. Handy \
                 after importing or pasting in data in bulk, where an invisible extra space can \
                 make two identical-looking owner names sort and group as if they were two \
                 different people.",
            ),
            Block::Sub("Deleting a record"),
            Block::P(
                "🗑 Delete removes the selected record and reclaims the storage space used by any \
                 documents that were attached to it. If you try to delete an account that other \
                 assets are still linked to, vaultis asks you to confirm first and tells you \
                 exactly how many other records point at it.",
            ),
            Block::Note(
                "Those links are deliberately NOT deleted along with the account — deleting the \
                 account leaves the linked asset's link showing as an unresolved id rather than \
                 silently rewriting a record you never opened. Nothing in this app ever deletes \
                 data you did not directly ask it to delete.",
            ),
            Block::Sub("Saves cannot be interrupted into a half-written mess"),
            Block::P(
                "Every save first writes a complete new copy of the file, then swaps it into place \
                 in one instant step. Losing power or the app crashing in the middle of that leaves \
                 you with either the OLD complete vault or the NEW complete vault — never a \
                 corrupted mixture of the two.",
            ),
        ],
    },
    Topic {
        id: "secrets",
        section: "Working with records",
        title: "Passwords: reveal, generate, copy",
        blurb: "How the passwords stored inside your vault are shown on screen, generated for you, and safely put on the clipboard.",
        body: &[
            Block::Sub("Revealing a masked password"),
            Block::P(
                "Passwords are masked (shown as dots) by default. There is a single screen-wide \
                 \"reveal all\" switch for this — on the Accounts tab and, separately, on the Real \
                 Estate tab — rather than a toggle on each individual field, so there is never any \
                 ambiguity about which passwords are currently visible on your screen.",
            ),
            Block::P(
                "Switching to a different tab resets reveal back to whatever \"Reveal all \
                 passwords by default\" is set to in Config. With that preference left off (the \
                 default), every tab re-masks itself the moment you leave it, so a password you \
                 revealed cannot linger visible on screen for someone glancing over your shoulder \
                 later.",
            ),
            Block::Sub("Generating a new, strong password"),
            Block::P(
                "Clicking 🎲 fills the field with a strong, randomly generated password and turns \
                 reveal on automatically so you can actually see and, if you like, write down what \
                 it generated. It REPLACES whatever was already typed in that field — copy the old \
                 value first if you might still need it.",
            ),
            Block::Sub("Copying a password to the clipboard"),
            Block::P(
                "Clicking 📋 copies the password to your clipboard, flagged along the way to be \
                 kept out of any clipboard-history tool running on your computer.",
            ),
            Block::Bullets(&[
                "The clipboard clears itself automatically 15 seconds after a copy.",
                "It is cleared again the moment the app exits, just to be sure.",
                "A clipboard-history tool that ignores that exclusion flag may still keep its own \
                 record of what was copied — the 15-second auto-clear only overwrites the live \
                 clipboard, it cannot reach into somebody else's separate log of it.",
            ]),
        ],
    },
    Topic {
        id: "searching",
        section: "Working with records",
        title: "Searching",
        blurb: "How the highlighted search box matches — anywhere inside the text, and even by how a name SOUNDS if you are not sure how to spell it.",
        body: &[
            Block::P(
                "The Accounts tab's filter row has a search box, drawn as an outlined pill shape \
                 with a magnifying glass so it visually stands apart from the plain dropdowns \
                 beside it. Type into it and the list narrows as you go — there is no button to \
                 press and nothing to submit.",
            ),
            Block::Sub("How to tell whether a search is currently active"),
            Block::P(
                "While a search is filtering the list, the box itself is tinted, its outline gets \
                 thicker, and a × appears inside it so you can clear it with one click. A \
                 \"filtered\" badge also appears in the filter row whenever anything at all — a \
                 search, or a dropdown — is hiding some rows from view. An unexpectedly short list \
                 with no obvious reason is the single most common \"where did my records go\" \
                 moment, so this state is made deliberately hard to miss rather than left for you \
                 to remember on your own.",
            ),
            Block::Sub("What the search box actually matches"),
            Block::Bullets(&[
                "The account's USERNAME or its TITLE — a match in either one keeps that record on \
                 screen.",
                "Case is ignored entirely: alice, Alice, and ALICE are treated as the same query.",
                "The letters you type may appear ANYWHERE inside the value, not only at its very \
                 start. \"elit\" finds \"Fidelity\"; \"heck\" finds \"Checking\".",
                "Sound-alike spellings match too: typing \"jonson\" finds Johnson, \"catherine\" \
                 finds Katherine, \"smith\" finds Smyth. The separate words inside an email address \
                 or handle are heard individually, so \"smith\" also finds alice.smyth@example.com.",
                "Typing several words NARROWS the results rather than widening them: EVERY word \
                 you type must be satisfied somewhere, so \"catherine smith\" finds Katherine \
                 Smyth, but not Katherine Jones.",
            ]),
            Block::Sub("Where the sound-alike matching deliberately stops"),
            Block::P(
                "Sound-alike matching only kicks in for words of three letters or more, and it \
                 never applies to digits at all. A short query like \"u2\", or a number like \
                 \"2024\", must therefore be typed exactly as it appears — which is what you want, \
                 since \"u1\" and \"u2\" are two entirely different logins, and no number \"sounds \
                 like\" another one.",
            ),
            Block::Note(
                "This matching is deliberately generous: it exists to help you find a name you \
                 cannot quite spell, so every so often it will offer up a record you were not \
                 actually looking for. The type, subtype, owner, and title dropdowns next to it \
                 are the exact, no-guessing tools — combine one of those with the search box to \
                 narrow things down hard. The dropdowns always stay in step with whatever the \
                 search is currently showing, so they will never offer you a choice that would \
                 leave the list empty.",
            ),
            Block::Sub("This same search box shows up elsewhere in the app"),
            Block::Bullets(&[
                "The \"Link an account…\" dropdown on an asset has this identical search box \
                 inside its popup — see “Linking assets and accounts”.",
                "This manual's own search box, in the top-right corner of the Help screen, matches \
                 topic titles and article text the same way, and also requires every word you \
                 type to be found.",
                "The keyboard-driven terminal interface has the same account search: press / to \
                 type a query, Enter to keep it, Esc to clear it. It shows up in the header there \
                 as find~\"…\".",
            ]),
        ],
    },
    Topic {
        id: "documents",
        section: "Working with records",
        title: "Documents: attaching and exporting",
        blurb: "How to get a file from your computer INTO the encrypted archive, and how to get a plain, readable copy back OUT of it again.",
        body: &[
            Block::Sub("Attaching a file"),
            Block::Steps(&[
                "Put the file's location on your computer into the \"Upload from\" box. Pasting it \
                 straight from your file manager works even if it comes wrapped in double quotes — \
                 see “Typing file and folder paths”.",
                "Optionally, type a subfolder name to help organise it once it is inside the vault.",
                "Optionally, set a different filename for it. Leave this blank to simply keep the \
                 original file's own name.",
                "Click the upload button. vaultis reads the file, encrypts it, and stores that \
                 encrypted copy inside the vault's document archive.",
            ]),
            Block::P(
                "The ORIGINAL file on your computer is left exactly where it was — it is not moved \
                 or deleted, the vault simply takes an encrypted copy of it. If that original was \
                 the only copy of something sensitive, it is up to you to delete it yourself \
                 afterwards if you want it gone.",
            ),
            Block::Sub("Where your documents actually end up being stored"),
            Block::P(
                "You do not have to think about folder structure — vaultis works it out for you \
                 automatically, owner-first, with the exact time of upload folded into the \
                 filename so nothing ever collides:",
            ),
            Block::Rows(&[(
                "Layout",
                "[<owner initials>/]<record type>[/<group>][/<your subfolder>]/<timestamp>_<filename>",
            )]),
            Block::P(
                "You only ever control the optional subfolder name and the filename; everything \
                 else in that path is handled for you, and exists specifically to stop documents \
                 belonging to different owners or different kinds of record from ever mixing \
                 together.",
            ),
            Block::Sub("Getting a document back out again"),
            Block::P(
                "Exporting writes a fully DECRYPTED, ordinary copy of the document into the export \
                 directory set once in Config, rebuilding its folder structure underneath that \
                 directory. You are never asked to pick a location each time you export — you set \
                 it once, in Config, and every Export button from then on writes there.",
            ),
            Block::Bullets(&[
                "An export never silently overwrites a file that is already there; it adds a _2, \
                 _3, and so on to the filename instead.",
                "Exporting works even in a read-only session — this is deliberately how an heir, \
                 who may never use write mode at all, actually gets documents out onto their own \
                 computer.",
            ]),
            Block::Warn(
                "Every exported file is a plain, unencrypted copy sitting in an ordinary folder on \
                 your disk, just like any other document — the protection the vault gave it is \
                 gone the moment it is exported. Put exported files somewhere you trust, and \
                 delete them again once you are done with them. vaultis reminds you of this right \
                 in the status line the moment an export finishes, not only here: the message \
                 turns red and leads with “⚠ UNENCRYPTED”, before it names the file, so the \
                 warning is readable even when a long path gets shortened to fit.",
            ),
        ],
    },
    Topic {
        id: "links",
        section: "Working with records",
        title: "Linking assets and accounts",
        blurb: "Tying an asset or liability to the login that actually manages it, and jumping straight between the two later.",
        body: &[
            Block::P(
                "An asset or liability can name the account (or accounts) that hold or service it \
                 — the brokerage login behind a portfolio, the mortgage servicer's login behind a \
                 house, the bank login behind a cash balance. This is purely a convenience for \
                 finding your way around later; it does not change either record's own data.",
            ),
            Block::Steps(&[
                "Open the asset on the Assets and Liabilities tab.",
                "In its 🔗 Linked accounts section, click open the \"➕ Link an account…\" dropdown.",
                "Type a few letters into the search box at the top of the popup to find the \
                 account you want, then click it.",
                "Save the asset. Links are stored ON the asset record itself, so they only take \
                 effect once you save it.",
            ]),
            Block::Sub("Finding the right account inside the dropdown"),
            Block::P(
                "The popup opens with its search box already focused, so you can simply start \
                 typing the moment it appears — no extra click needed. The list underneath it \
                 narrows to matching accounts as you type, and automatically scrolls the best \
                 match into view. It matches exactly the same way the Accounts search box does: \
                 the letters can appear anywhere in the entry, not only at the very start, and a \
                 sound-alike spelling still finds it — see “Searching” for the full rules.",
            ),
            Block::Bullets(&[
                "Only accounts that are NOT already linked to this particular asset are offered, \
                 so it is impossible to add the same link twice by accident.",
                "Whatever you typed is forgotten the moment the popup closes — the next time you \
                 open it, you get the full list again rather than a stale, half-remembered filter.",
                "If nothing matches what you typed, the popup says so plainly rather than simply \
                 appearing empty and leaving you unsure whether it is broken.",
            ]),
            Block::Sub("Looking the other way — from the account back to its assets"),
            Block::P(
                "The Accounts form shows every asset that links back to whichever account you are \
                 currently looking at, each with a button that jumps you straight to it. Jumping \
                 switches tabs for you and clears any filter that would have hidden the asset you \
                 asked for, so you always land on exactly the record you clicked, not on an empty \
                 filtered list. Links can only be added or removed from the ASSET's side — which \
                 is exactly why this reverse view on the account is read-only.",
            ),
            Block::Note(
                "Links are stored using the record's internal id, not its name, so renaming either \
                 side of a link leaves the link itself perfectly intact. Deleting a linked account \
                 leaves the link on the asset showing as a raw, unresolved id rather than silently \
                 rewriting the asset record behind your back.",
            ),
        ],
    },
    Topic {
        id: "history",
        section: "Working with records",
        title: "Record history",
        blurb: "Every single record quietly keeps its own log of exactly what changed, and when — worth a glance before trusting an old entry.",
        body: &[
            Block::P(
                "Every record carries its own private change history, shown at the very bottom of \
                 its form: what was created, which fields were edited, and exactly when each of \
                 those things happened. It is written automatically every time you save — there is \
                 nothing you need to turn on.",
            ),
            Block::P(
                "History is what tells you whether the login you are looking at was checked and \
                 confirmed working last month, or has not been touched in a decade — worth a quick \
                 glance before you trust an old credential enough to actually use it.",
            ),
            Block::Note(
                "History accumulates over time and slowly makes the vault file larger. The \
                 command-line `compact` tool can trim it — either all of it, or everything before a \
                 date you choose — once that starts to matter; the vault-level audit log itself is \
                 always kept regardless. See “Keeping the vault small”.",
            ),
        ],
    },
    // --- Settings & maintenance ---------------------------------------------
    Topic {
        id: "config",
        section: "Settings & maintenance",
        title: "Config: every setting explained",
        blurb: "A plain-language walkthrough of every single option in Settings: appearance, what tabs look like by default, type lists, exports, storage, and redundancy.",
        body: &[
            Block::Sub("Appearance"),
            Block::P(
                "Sixteen color themes to choose from: Light, Dark, High contrast, Solarized, \
                 Sepia, Nord, Dracula, Gruvbox Dark, Gruvbox Light, Rosé Pine, Catppuccin Mocha \
                 (the default), Catppuccin Latte, Tokyo Night, One Dark, Everforest, and Zenburn. \
                 Your choice applies immediately and is saved in your vaults folder, so it comes \
                 back the next time you open a vault from there.",
            ),
            Block::Sub("View defaults"),
            Block::P("Two small preferences that decide how each tab looks the moment you first arrive on it:"),
            Block::Rows(&[
                ("Group assets by default", "Whether the Assets tab opens already showing its tree view."),
                ("Group accounts by default", "Whether the Accounts tab opens already showing its tree view."),
            ]),
            Block::Note(
                "There is deliberately no \u{201c}reveal all passwords by default\u{201d} setting. Passwords always \
                 start hidden, and revealing them is a per-session choice you make each time. \
                 Because prefs.conf sits in your vaults folder rather than inside the encrypted \
                 vault, anyone who could edit that folder without knowing your passwords could \
                 otherwise have switched masking off for you.",
            ),
            Block::Sub("Asset / Liability types · Account types & subtypes"),
            Block::P(
                "These are the dropdown lists used throughout the record forms. Add a new type or \
                 subtype using the boxes provided; remove one with its × button. A type currently \
                 in use by a record cannot be deleted, and an account type that still has subtypes \
                 under it must have those subtypes removed first — anything that IS safe to remove \
                 right now is marked \"unused\" for you.",
            ),
            Block::Note("These lists live INSIDE the encrypted vault itself — there are no separate configuration files sitting elsewhere that could get lost."),
            Block::Sub("Export directory"),
            Block::P(
                "This is the one folder every Export button writes its decrypted copy into. It is \
                 held for the CURRENT SESSION only and is never written to disk, so you set it \
                 again each time you open the app — and it can be set even during a read-only \
                 session. Paste the folder's path in and click its Set button — a path wrapped in \
                 double quotes is accepted automatically, see “Typing file and folder paths”. \
                 Clearing the box turns exporting off again entirely.",
            ),
            Block::Note(
                "It is deliberately not remembered. This folder receives files with no encryption \
                 at all — a CSV export spells out every password — and the only settings file \
                 vaultis writes sits in your vaults folder, where anyone who can reach that folder \
                 could edit it without knowing your passwords. Choosing the destination fresh each \
                 session is what stops someone else from choosing it for you.",
            ),
            Block::Sub("Backup"),
            Block::P(
                "Copies the whole encrypted vault and its document archive into a new, \
                 timestamped folder under whichever destination you give it — that destination box \
                 also accepts a quoted, pasted path. Nothing is decrypted in the process. See \
                 “Backups and recovery” for the full picture.",
            ),
            Block::Sub("Storage — volume size"),
            Block::P(
                "Your uploaded documents are packed together into fixed-size encrypted blocks \
                 called volumes; once the current one fills past this size, a new one starts \
                 automatically. Changing this setting only affects where FUTURE documents get \
                 packed — it never touches anything already stored.",
            ),
            Block::Sub("Vault file redundancy (advanced)"),
            Block::P(
                "Keeps extra encrypted backup copies of the small vault file sitting right beside \
                 it: one same-generation mirror, plus a number of previous generations you choose \
                 — which doubles as a built-in \"undo\" for your very last save if something goes \
                 wrong with it. Set it to 0 to turn this off entirely.",
            ),
            Block::Warn(
                "Redundancy only protects you against a single DAMAGED file — it does nothing at \
                 all if the disk itself is lost, stolen, or destroyed in a fire. It is a safety net \
                 for corruption, not a substitute for keeping real backups stored somewhere else \
                 entirely. See “Backups and recovery”.",
            ),
            Block::Sub("Sync types from records"),
            Block::P(
                "Scans every single record in the vault and adds any type or subtype word it finds \
                 in use that is missing from the dropdown lists above. Useful after pulling records \
                 in from a second vault whose type lists were set up slightly differently from this \
                 one's.",
            ),
        ],
    },
    Topic {
        id: "merge",
        section: "Settings & maintenance",
        title: "Updating from another vault",
        blurb: "Pull newer records — and the documents attached to them — in from a second copy of this vault. Always one-way, and never deletes anything.",
        body: &[
            Block::P(
                "If you keep a copy of this vault on more than one computer, this feature brings \
                 changes made on the OTHER copy into the one you have open now. It looks at every \
                 record that is newer, or entirely new, in the other vault, and pulls it in — along \
                 with whichever documents that record needs.",
            ),
            Block::Bullets(&[
                "One-way only: the OTHER vault is only ever opened read-only during this process, and is never itself changed.",
                "Additive only: nothing already in THIS vault is ever deleted by an update.",
                "Previewed first: the exact list of every change is shown to you before anything is actually applied.",
            ]),
            Block::Steps(&[
                "In Config, click \"Update from another vault…\" (write mode only).",
                "Choose the other vault's folder — pasting a quoted path is fine — and type ITS \
                 own two passwords, not this vault's.",
                "Read through the preview carefully: every record about to be added or updated, \
                 plus whichever documents come along with them.",
                "Click Apply, or simply go back without changing anything if you are not ready.",
            ]),
            Block::Note(
                "Back this vault up before applying an update, just in case. The equivalent \
                 command-line tool is `vaultis update-from OTHER [DIR] --dry-run`, which prints the \
                 exact same preview without actually changing anything.",
            ),
            Block::P(
                "After an update finishes, run \"Sync types from records\" in Config if the \
                 records that came in use type or subtype words your own vault's lists do not have \
                 yet.",
            ),
        ],
    },
    Topic {
        id: "backups",
        section: "Settings & maintenance",
        title: "Backups and recovery",
        blurb: "What to actually copy, how often, and — importantly — the one thing no backup in the world can save you from.",
        body: &[
            Block::P(
                "The Backup button in Config copies the ENTIRE encrypted vault — the record file, \
                 the manifest, and the whole document archive — into a new, timestamped folder. \
                 Nothing is decrypted along the way, so a backup is exactly as safe to store \
                 anywhere as the vault itself already is.",
            ),
            Block::Sub("A simple routine worth actually following"),
            Block::Bullets(&[
                "Back the vault up after any session where you changed something.",
                "Keep at least one copy on separate physical media from your computer, and ideally one copy off-site entirely (a different building, a safe-deposit box, a trusted relative's house).",
                "Always back up the whole vault FOLDER, not pieces of it. A vault.pmv file without its matching volume/ folder has your records but none of your actual documents.",
                "Test a backup every so often by opening it read-only, just to confirm it actually works. A backup you have never tested opening is a hope, not a real backup.",
            ]),
            Block::Warn(
                "Your two passwords are NOT stored inside the backup — they never are, anywhere. A \
                 perfect backup paired with a forgotten password is exactly as unrecoverable as a \
                 lost vault with no backup at all. Store the passwords with the same care you give \
                 the data itself, somewhere your executor will genuinely be able to find them.",
            ),
            Block::Sub("Recovering from a backup"),
            Block::P(
                "To restore, simply copy a backup folder back to wherever you keep vaults and open \
                 it exactly like any other vault. If redundancy is turned on (see “Config: every \
                 setting explained”), a damaged vault.pmv can sometimes even be repaired IN PLACE \
                 from its mirrored copies, without needing to reach for a backup at all.",
            ),
        ],
    },
    Topic {
        id: "maintenance",
        section: "Settings & maintenance",
        title: "Keeping the vault small",
        blurb: "Two reasons the vault file slowly grows over time, and the command-line tool that shrinks it back down.",
        body: &[
            Block::P("Two things make the vault grow larger than what it actually holds, over time:"),
            Block::Bullets(&[
                "Edit history — every single save appends one more entry to that record's log, forever, by default.",
                "Leftover document blocks — replacing or detaching a document leaves its old \
                 encrypted blocks sitting inside the archive rather than instantly rewriting the \
                 whole volume just for that one change.",
            ]),
            Block::P("The `compact` command reclaims both of these. It is a command-line-only operation because it has to rewrite the entire vault:"),
            Block::Rows(&[
                ("vaultis compact [DIR] --volume", "Re-packs the document archive from scratch, dropping the leftover blocks."),
                ("vaultis compact [DIR] --json --history-all", "Drops ALL recorded edit history, for every record."),
                ("vaultis compact [DIR] --json --history-before YYYY-MM-DD", "Keeps history from that date onward only, discarding anything older."),
                ("--dry-run", "Reports exactly what would be reclaimed, without actually changing anything."),
            ]),
            Block::Note(
                "Compaction backs the vault up first automatically by default, and is crash-safe \
                 the same way saves are: an interruption partway through leaves you with either \
                 the old vault or the fully compacted one, never a half-finished mixture. The \
                 vault-level audit log itself is always preserved regardless of any of these \
                 options.",
            ),
        ],
    },
    // --- Reference -----------------------------------------------------------
    Topic {
        id: "security",
        section: "Reference",
        title: "How your data is protected",
        blurb: "A plain-language summary of the encryption, how secrets are handled in memory, and — just as important — what none of this can protect you from.",
        body: &[
            Block::Sub("The encryption, in plain terms"),
            Block::Bullets(&[
                "Your two passwords are combined through a deliberately slow, memory-hungry \
                 scrambling process called Argon2id, specifically chosen to make guessing \
                 passwords by brute force extremely expensive — full detail in “For the \
                 technically curious: why this is safe”.",
                "The vault, its document index, and every document volume are all sealed with a \
                 strong, modern cipher (XChaCha20-Poly1305) that both scrambles the data AND \
                 detects any tampering with it.",
                "Even the small header at the start of the file — which holds the technical \
                 parameters used to lock it — is protected against tampering, so nobody can quietly \
                 weaken the lock on a vault file and hand it back to you.",
                "Reading data that has been damaged or tampered with FAILS outright rather than \
                 showing you something wrong — the app never displays data it could not first \
                 verify as genuine and untouched.",
            ]),
            Block::Sub("How secrets are handled while the app is running"),
            Block::Bullets(&[
                "Passwords and any decrypted secret are overwritten with zeros in memory the \
                 instant they are no longer needed.",
                "On desktop, the memory holding secrets is locked so the operating system is \
                 not allowed to write it out to the disk's swap space.",
                "The clipboard clears itself automatically 15 seconds after any copy, and again \
                 when the app exits.",
            ]),
            Block::Sub("What none of this can protect you from"),
            Block::P(
                "Encryption protects the vault while it is sitting at rest, closed. It cannot \
                 protect a computer that is already compromised: malware running as you, with the \
                 vault unlocked on screen, sees exactly what you see — the same way it would with \
                 any other program open. It also, obviously, cannot protect plaintext that you \
                 yourself have chosen to export out onto the disk.",
            ),
            Block::Warn(
                "In practice, the weakest point is almost never the cryptography itself. It is a \
                 password written down somewhere careless, or a folder of exported, decrypted \
                 documents left behind and forgotten about.",
            ),
            Block::Note(
                "This is the plain-language version. For the actual algorithms, the reasoning \
                 behind them, and specifically how this holds up against a future quantum \
                 computer, see “For the technically curious: why this is safe”.",
            ),
        ],
    },
    Topic {
        id: "paths",
        section: "Reference",
        title: "Typing file and folder paths",
        blurb: "Every path box in the app accepts a pasted, quoted path exactly as your file manager gives it to you — and the couple of things a path box will not do.",
        body: &[
            Block::P(
                "Several boxes throughout the app ask for a location on your disk rather than \
                 vault content — a folder, or a specific file. They all behave the same way, so \
                 whatever you learn about one applies equally to all the others:",
            ),
            Block::Rows(&[
                ("Vaults folder", "The start page's folder that vaultis scans for your vaults."),
                ("Upload from", "The specific file you want to attach to a record."),
                ("Export directory", "Where every Export button writes its decrypted copies (set in Config)."),
                ("Backup destination", "Where a new backup folder gets created (set in Config)."),
                ("Other vault's folder", "The source vault when updating from another vault (set in Config)."),
            ]),
            Block::Sub("Pasting a path that came wrapped in quotes"),
            Block::P(
                "File managers and shells often hand you a path already wrapped in double quotes \
                 whenever it contains spaces — Windows Explorer's right-click \"Copy as path\" \
                 always does this, for example. Paste it in exactly as it came: a single matching \
                 PAIR of quotes around the whole path is recognised and removed automatically, so \
                 pasting \"C:\\Users\\me\\My Vaults\" opens the folder it names, spaces and all — the \
                 spaces inside the path are always kept exactly as they are, never touched.",
            ),
            Block::Bullets(&[
                "Any stray blank space around the outside is trimmed too, so an accidental extra \
                 space from copy-pasting is harmless.",
                "The cleaned-up value — quotes and outer spaces removed — is what actually gets \
                 remembered, so a quoted path you paste in once does not come back still wrapped \
                 in quotes the next time you open the app.",
                "A single quote mark at only ONE end is left completely alone, because it is a \
                 perfectly legal character inside a filename on Linux and macOS — so a folder \
                 genuinely named \"weird is still reachable exactly as typed.",
                "Ordinary single quotes ('like this') are never stripped — they are treated as \
                 plain, ordinary characters in a filename, not a quoting convention any file \
                 manager actually uses.",
            ]),
            Block::Sub("What a path box will NOT do for you"),
            Block::Bullets(&[
                "It will not expand shortcuts like ~ or environment variables such as \
                 %USERPROFILE% or $HOME — type or paste the FULL path instead.",
                "A relative path (one that does not start from the very top of your drive) is \
                 resolved against whatever folder the program happened to be launched from, which \
                 is rarely what you actually meant — an absolute, full path is always safer.",
                "\"Upload from\" needs the path to an actual FILE, including its filename at the \
                 end — pointing it at just a folder will not work.",
            ]),
            Block::Note(
                "The separate command-line tools take paths as ordinary typed arguments instead, \
                 where your own shell strips the quotes before the program ever even sees them — \
                 quote any path containing spaces there the normal way your shell expects.",
            ),
        ],
    },
    Topic {
        id: "keys",
        section: "Reference",
        title: "Keyboard and mouse",
        blurb: "Every keyboard shortcut and mouse behaviour this window recognises, in one place.",
        body: &[
            Block::Rows(&[
                ("Up / Down arrow", "Moves to the previous or next record in a flat (non-grouped) list, scrolling it into view automatically."),
                ("Click a row", "Opens that record into the form on the right."),
                ("Click a group header", "Expands or collapses that branch, when viewing a grouped tree."),
                ("Tab / Shift+Tab", "Moves the cursor between fields in the form."),
                ("Enter", "Submits the unlock screen — the same as clicking Unlock or Create."),
                ("Ctrl+C", "Copies whatever text is selected — this works even inside a read-only field."),
                ("Ctrl+V", "Pastes — this is how a path copied from your file manager gets into a path box."),
                (
                    "Just start typing",
                    "Inside the \"Link an account…\" dropdown: its search box already has focus \
                     the instant the popup opens, so your very first keystroke starts filtering \
                     the list immediately.",
                ),
                ("Esc", "Closes an open dropdown popup without choosing anything from it."),
                ("Click away from a popup", "Also closes it; a link search box forgets whatever you typed once it closes."),
                ("Hover over a button", "Shows a small tooltip explaining exactly what that button does."),
                ("Hover over the vault name", "Shows the full folder path of the vault that is currently open."),
            ]),
            Block::Note(
                "This graphical window is built mouse-first, on purpose. The separate terminal \
                 interface (`vaultis --tui`) is the keyboard-driven one instead: there, the number \
                 keys 1–9 jump between tabs, n and d create and delete a record, g toggles grouped \
                 view, r reveals passwords, / starts a search, and Ctrl+S saves.",
            ),
        ],
    },
    Topic {
        id: "cli",
        section: "Reference",
        title: "Command line",
        blurb: "Everything the console version of the program can do that this graphical window cannot — mainly bulk, one-off jobs.",
        body: &[
            Block::P(
                "The console program (`vaultis`) opens the exact same vaults as this window, and \
                 adds a handful of bulk operations on top. DIR means the vault's folder; leaving it \
                 out simply uses your default one. Every single command prompts you for the two \
                 passwords before doing anything.",
            ),
            Block::Rows(&[
                ("vaultis [DIR]", "Launches this same graphical app, in read-only mode."),
                ("vaultis --write [DIR]", "Launches it able to make changes instead."),
                ("vaultis --tui [DIR]", "Launches the keyboard-driven terminal interface instead of this window."),
                ("vaultis decrypt [DIR]", "Prints the ENTIRE decrypted vault as JSON text — every secret, in plain readable text."),
                ("vaultis manifest [DIR]", "Prints the decrypted index of your uploaded documents."),
                ("vaultis extract [DIR] OUT", "Decrypts every single stored document out into the folder OUT."),
                ("vaultis backup [DIR] DEST", "Copies the still-encrypted vault into a new, timestamped folder under DEST."),
                ("vaultis export-tree [DIR] OUT", "Writes a complete, fully decrypted mirror of the whole vault."),
                ("vaultis import-tree SRC [DIR]", "Builds a BRAND-NEW encrypted vault, with new passwords you choose, out of such a mirror."),
                ("vaultis update-from OTHER [DIR]", "Pulls newer records in from another vault; add --dry-run to preview it first without changing anything."),
                ("vaultis compact [DIR] …", "Reclaims wasted space. See “Keeping the vault small”."),
                ("vaultis --help", "The full, authoritative list of every option this tool supports."),
            ]),
            Block::Warn(
                "decrypt, extract, and export-tree all write or print your data with absolutely no \
                 encryption at all. Redirecting decrypt's output into a file, for example, puts \
                 every single password onto your disk in plain, readable text.",
            ),
            Block::Note(
                "export-tree and import-tree round-trip with each other, which is deliberately the \
                 \"escape hatch\": your data can always be taken completely out of this program's \
                 format, in full, if you ever needed to.",
            ),
        ],
    },
    Topic {
        id: "demo-vault",
        section: "Reference",
        title: "Trying it out on a sample vault",
        blurb: "A throwaway practice vault full of invented, fake data, built by the build script — a safe place to click around and learn without touching your real information.",
        body: &[
            Block::P(
                "If you built the program yourself from source, one single command builds it AND \
                 leaves you with a fully filled-in practice vault to click around in — \
                 scripts/build.sh on Linux or macOS, scripts\\build.bat on Windows. The two are \
                 identical twins of each other: same flags, same defaults, same demo passwords.",
            ),
            Block::Rows(&[
                ("(no flags)", "Builds the program, then creates the sample vault too, if it is not already there."),
                ("--release", "The same thing, using the faster, optimised build."),
                ("--fresh", "Throws away any existing sample vault and builds a brand-new one from scratch."),
                ("--sample-dir DIR", "Puts the sample vault somewhere else on disk instead of the default location."),
                ("--no-sample", "Just builds the program; skips creating the sample vault."),
                ("--install-rust", "Installs the Rust programming toolchain automatically if it is missing, without asking first."),
                ("-- <cargo args>", "Everything typed after a bare -- is passed straight on to cargo build."),
                ("--help", "Prints this same list of options."),
            ]),
            Block::Note(
                "No Rust toolchain on your computer yet? The script checks for one before it \
                 builds anything, and offers to install it for you using the official rustup \
                 installer — it always asks first, and the default answer if you just press Enter \
                 is no. A toolchain that IS present but too old is reported plainly, rather than \
                 failing later with a confusing compiler error that does not explain the real \
                 problem.",
            ),
            Block::P(
                "When it finishes, it prints — as the very last thing, after all the build output — \
                 exactly where the sample vault ended up and the two passwords that open it. By \
                 default that is a sample-vault folder under target/, unlocked with the two \
                 passwords sample1 and sample2.",
            ),
            Block::Sub("Opening it in one click"),
            Block::P(
                "You do not have to type any of that in. Whenever a sample vault exists where the \
                 build script puts it, the lock screen shows a “Sample vault” button beneath the \
                 password fields: clicking it fills in the folder and both demo passwords and \
                 opens the vault straight away. The button only appears when there really is a \
                 sample vault to open, so on an ordinary installed copy of the program you will \
                 never see it.",
            ),
            Block::Note(
                "The button only ever OPENS: \
                 if the sample vault has been deleted since the program started (running \
                 `cargo clean` removes it), the button says so rather than quietly creating a new \
                 vault with the demo passwords.",
            ),
            Block::Sub("What is actually inside it"),
            Block::P(
                "Every single tab is filled in — urgent notes, instructions, trust and will \
                 entries, assets and liabilities linked to accounts, accounts with types and \
                 subtypes, a property with all four of its portal logins, tax filings, and general \
                 documents — including real attached documents, so the encrypted archive, \
                 exporting, and the Summary tab's totals all have something real to show you.",
            ),
            Block::Bullets(&[
                "Everything inside it is fiction: invented people, invented institutions, and \
                 obviously fake placeholder \"passwords\". Nothing in it is a working login for \
                 anything real, anywhere.",
                "Its two master passwords are deliberately trivial to guess, because it is only a \
                 demo — which is exactly why it must never be used to hold anything real.",
                "It lives under the target/ build folder, so running `cargo clean` removes it along \
                 with the rest of the build output.",
                "Running the script again leaves an EXISTING sample vault untouched, so any edits \
                 you made while exploring it survive between runs. Use --fresh specifically when \
                 you want a brand-new, clean one instead.",
            ]),
            Block::Warn(
                "Never put real information into the sample vault, and never reuse sample1 or \
                 sample2 as passwords on a vault of your own. Treat the whole sample-vault folder \
                 as disposable, throwaway practice material.",
            ),
            Block::Note(
                "This is also the fastest way to show somebody else what the program actually does, \
                 or to practise doing a restore from backup, without ever exposing any of your own \
                 real records to them.",
            ),
        ],
    },
    Topic {
        id: "troubleshooting",
        section: "Reference",
        title: "Troubleshooting",
        blurb: "The messages people actually run into, in plain English, and exactly what each one really means.",
        body: &[
            Block::Rows(&[
                (
                    "The passwords are rejected but you are sure they are right",
                    "Double-check the ORDER you typed them in — they are not interchangeable, see \
                     “The two passwords”. Check your keyboard layout and whether Caps Lock is on. \
                     Confirm you are actually opening the vault folder you think you are. Note that \
                     this exact same message also appears for a damaged file, on purpose.",
                ),
                (
                    "\"already open\" when you launch the app",
                    "A window for this vault is already open somewhere; vaultis has simply brought \
                     it to the front instead of opening a second copy. Look for it on another \
                     desktop/workspace, or minimised in your taskbar.",
                ),
                (
                    "A writable (--write) session refuses to start",
                    "Another writable session already holds the single-writer lock on this vault. \
                     Close that other window. If nothing is actually running, the lock was probably \
                     left behind by a crash — it clears itself automatically once the app confirms \
                     that stale process is really gone.",
                ),
                (
                    "Nothing on screen seems to be editable",
                    "The session is read-only. Look for the orange 🔒 badge in the top bar; if you \
                     need to make changes, close the window and relaunch with --write. See \
                     “Read-only vs. write mode”.",
                ),
                (
                    "A save or an upload failed",
                    "Read the red banner across the top of the window — it carries the real reason. \
                     Usually this is a full disk, or a permissions problem on the vault's folder. \
                     The vault itself is intact either way: saves are all-or-nothing, so a failed \
                     save changed absolutely nothing.",
                ),
                (
                    "Export seemed to do nothing visible",
                    "Check the export directory set in Config — that is exactly where the file went, \
                     silently and successfully. An existing file there is never overwritten, so also \
                     look for one with a _2 suffix added to its name.",
                ),
                (
                    "An upload says it cannot find the file",
                    "The box needs the WHOLE path, not just the filename by itself — and it has to \
                     point directly at a file, not at a folder. A pasted, quoted path is fine; ~ and \
                     %USERPROFILE% are not expanded automatically. See “Typing file and folder \
                     paths”.",
                ),
                (
                    "A folder box cannot seem to find the folder you typed",
                    "Look for a stray leftover character from copying it: a matching PAIR of double \
                     quotes is stripped for you automatically, but a single leftover quote at only \
                     one end is treated as a real part of the folder's name. An absolute (full) path \
                     is always the safest bet — a relative one gets resolved against wherever the \
                     program happened to be started from.",
                ),
                (
                    "The list on screen is unexpectedly, mysteriously empty",
                    "A filter or the search box is almost certainly still active somewhere — look \
                     for the \"filtered\" badge and the tinted search box. Click Clear, or the × \
                     sitting inside the search box itself.",
                ),
                (
                    "The search turned up a record you did not ask for",
                    "The search box also matches names that merely SOUND like what you typed, so a \
                     different spelling can legitimately show up. Type a few more letters, or narrow \
                     things down with the type/owner dropdowns instead. See “Searching”.",
                ),
                (
                    "The search cannot seem to find a short username",
                    "Words shorter than three letters are only ever matched literally, never by \
                     sound — searching \"u2\" finds only an exact \"u2\". Try searching the title \
                     instead if the username itself is short.",
                ),
                (
                    "A link is showing as a raw, meaningless id instead of a name",
                    "The account it used to point at has been deleted. Edit the asset to either \
                     remove that stale link, or point it at a different account instead.",
                ),
            ]),
        ],
    },
    Topic {
        id: "faq",
        section: "Reference",
        title: "Questions people ask",
        blurb: "Short, direct answers about recovery, sharing this vault, trusting the cloud, and what to do next.",
        body: &[
            Block::Sub("Can a lost password ever be recovered?"),
            Block::P("No. Not by you, not by anyone else, ever. There is no reset link, no hidden backdoor, and no support channel anywhere that can help — see “The two passwords” for exactly why that is deliberate."),
            Block::Sub("Does anything ever leave this computer on its own?"),
            Block::P(
                "Only whatever YOU explicitly choose to export yourself. This program contains no \
                 network code whatsoever — it is physically incapable of phoning home, syncing, or \
                 updating itself, because that code was simply never written into it.",
            ),
            Block::Sub("Is it safe to keep the vault in cloud storage (Dropbox, iCloud, etc.)?"),
            Block::P(
                "The files themselves are strongly encrypted, so a copy sitting in ordinary cloud \
                 storage is a reasonable form of off-site backup. Just do not let two different \
                 computers both actively write to that same synced copy at the same moment, and \
                 never, ever store the two passwords in that same cloud location.",
            ),
            Block::Sub("How should I actually hand this over to my executor?"),
            Block::P(
                "Tell them plainly, ahead of time: that this vault exists, exactly where its folder \
                 is, where each of the two passwords is kept, and that they should open it \
                 READ-ONLY rather than in write mode. Point them straight at the URGENT tab — it \
                 should be the very first thing they read.",
            ),
            Block::Sub("What if this program itself no longer exists by the time it's needed?"),
            Block::P(
                "Keep a copy of the program itself stored alongside your backup, just in case. \
                 Failing that, `export-tree` turns any vault into an ordinary folder of plain files \
                 and JSON text that essentially any future tool could read — your data is never \
                 permanently trapped inside this one program's format.",
            ),
            Block::Sub("Why does the search sometimes find a name I did not even type?"),
            Block::P(
                "Because it also deliberately matches names that merely SOUND like your query — \
                 that is on purpose, so that an heir who has only ever heard a name spoken out loud \
                 can still find it without knowing the spelling. Type more letters, or use the exact \
                 dropdowns instead, to narrow it down. See “Searching”.",
            ),
            Block::Sub("Can I practise using this without touching my real vault?"),
            Block::P(
                "Yes. If you built the program from source, scripts/build.sh leaves you a sample \
                 vault full of entirely invented data, and prints both its location and its two demo \
                 passwords once it finishes. See “Trying it out on a sample vault”.",
            ),
            Block::Sub("What is actually a sensible routine to follow?"),
            Block::Bullets(&[
                "Open the vault read-only by default, and only in write mode when you genuinely intend to change something.",
                "Re-read the URGENT and Instructions tabs about once a year, so they stay accurate.",
                "Back up after any session where you changed anything, and keep at least one copy off-site.",
                "Check every so often that your executor still actually knows where the two passwords are kept.",
            ]),
        ],
    },
    Topic {
        id: "technical-safety",
        section: "Reference",
        title: "For the technically curious: why this is safe",
        blurb: "The actual cryptography behind the plain-language claims made everywhere else in this manual — the algorithms, the numbers, why it holds up against a future quantum computer, and precisely why the whole program is offline.",
        body: &[
            Block::P(
                "Everywhere else, this manual deliberately avoids jargon. This one article is the \
                 exception: it is written for a reader who wants the real technical detail behind \
                 “How your data is protected”, and who would rather be told exactly why something \
                 is safe than simply be reassured that it is.",
            ),
            Block::Sub("The two passwords, turned into one key"),
            Block::P(
                "Both of your passwords are run through Argon2id, a \"memory-hard\" key-derivation \
                 function: rather than being fast (which is what you want for logging in, but \
                 exactly what an attacker guessing millions of passwords wants too), it is \
                 deliberately slow and, more importantly, forces a large chunk of RAM to be used \
                 for every single attempt. By default this is 64 MiB of memory and 3 passes over \
                 it, run TWICE — once per password, with the first password's derived key chained \
                 into the derivation of the second, which is why the two are not interchangeable. \
                 Memory-hardness matters because it is expensive to parallelise: building a \
                 thousand machines to guess passwords in parallel means buying a thousand times the \
                 RAM as well, not just a thousand times the raw speed, which is far more costly than \
                 speeding up a purely computational hash.",
            ),
            Block::Note(
                "This cost is fixed into a vault when it is CREATED and cannot be changed \
                 afterwards without rebuilding the vault from scratch. It can be raised at creation \
                 time (VAULTIS_KDF_MCOST_MIB / VAULTIS_KDF_TCOST environment variables, 1–512 MiB \
                 and 1–16 passes), but think hard before doing so: that cost is paid on every \
                 single open, forever, on every device — including whatever an executor may be \
                 using years from now. Extra password length is free at open time and buys far more \
                 real security than raising this knob does; \"my executor could not open it\" is a \
                 far more likely failure than a brute-force attack.",
            ),
            Block::Sub("Locking the contents: XChaCha20-Poly1305"),
            Block::P(
                "The single key produced above locks the vault, its document index (manifest), and \
                 every document volume using XChaCha20-Poly1305 — an AEAD cipher, meaning \
                 \"Authenticated Encryption with Associated Data\". In plain terms: it does not just \
                 scramble your data, it also seals it with a cryptographic checksum that can only \
                 be produced by someone holding the correct key. Flip even a single bit anywhere in \
                 an encrypted file and decryption fails outright rather than quietly handing back \
                 corrupted or subtly altered data — this is tested directly, by a check that flips \
                 every single bit of a valid vault file in turn and confirms every one of those is \
                 rejected. The file's header — the parameters, salt, and nonce needed to even begin \
                 decrypting it — is authenticated too, so nobody can quietly weaken those settings \
                 on a copy of your vault and hand it back to you unnoticed. And a wrong password \
                 produces the exact same failure as a genuinely corrupted file, which specifically \
                 stops the app from ever being used as a password-guessing oracle.",
            ),
            Block::Sub("Encryption strength: what \"256-bit\" actually buys you"),
            Block::P(
                "XChaCha20-Poly1305 uses a full 256-bit key. To put that in perspective: correctly \
                 guessing a random 256-bit key by brute force would take dramatically longer than \
                 the current age of the universe, even for an attacker with every computer on Earth \
                 working together. In real life, nobody attacks a key like that directly — they \
                 attack the weaker of the two things standing in front of it, which is almost always \
                 the passwords chosen by the person who set the vault up, not the cipher itself.",
            ),
            Block::Sub("Quantum computers: the honest answer"),
            Block::P(
                "This is a fair question for anything meant to stay secret for decades, since a \
                 file copied today could simply be kept until a more powerful computer exists — \
                 known as \"harvest now, decrypt later\". The short answer here is that this design \
                 holds up well, and for a fairly unglamorous, structural reason rather than a clever \
                 trick.",
            ),
            Block::Bullets(&[
                "Shor's algorithm — the quantum algorithm that actually breaks encryption used \
                 across the internet today — only breaks PUBLIC-KEY cryptography (things like RSA, \
                 Diffie-Hellman, and elliptic curves), which is what secures things like recorded \
                 web traffic and encrypted messengers. vaultis contains NO public-key cryptography \
                 at all — there is no key exchange and nothing here for Shor's algorithm to target.",
                "Grover's algorithm is the one that DOES apply to a cipher like this one, but it \
                 only offers a quadratic speed-up — in effect, roughly HALVING the key strength, not \
                 breaking it outright. Applied to a 256-bit key, that still leaves about 128 bits of \
                 effective security, which is precisely the target researchers already consider \
                 \"quantum-safe\" for a symmetric cipher (it is exactly why standards bodies point at \
                 256-bit keys specifically). There is nothing to \"upgrade\" here: the newer \
                 post-quantum algorithms you may have read about (ML-KEM/Kyber, ML-DSA/Dilithium) \
                 replace public-key exchange and signatures, neither of which this program uses in \
                 the first place.",
                "Argon2id's memory-hardness helps here too, in a second, independent way: running \
                 Grover's algorithm against a memory-hard function would require an attacker to hold \
                 that entire chunk of memory in quantum superposition throughout the whole search — \
                 quantum memory at that scale is far beyond anything remotely on the horizon, which \
                 makes memory-hard password hashing one of the worst realistic targets for a \
                 quantum speed-up.",
            ]),
            Block::P(
                "The honest bottom line: the realistic way a vault harvested today eventually gets \
                 read is not a quantum computer arriving decades from now — it is that the two \
                 passwords were guessable, or that the plaintext leaked from a device while the \
                 vault happened to be unlocked. The single strongest thing you can personally do \
                 about \"harvest now, decrypt later\" is simply not letting a copy be harvested in \
                 the first place: keep backups on media you physically control, and choose two long, \
                 genuinely unique passwords.",
            ),
            Block::Sub("Why the whole program is offline — and why that specifically matters"),
            Block::P(
                "There is no network code anywhere in this codebase — not a setting you can find \
                 and switch off, but code that was simply never written. Concretely: the program \
                 never opens a network connection, never listens for one, cannot check for updates \
                 on its own, and on Android does not even request the INTERNET permission at all — \
                 so the operating system itself would refuse a connection attempt even if the app \
                 somehow tried to make one.",
            ),
            Block::P(
                "This matters because encryption only protects data that is AT REST (sitting \
                 encrypted on disk) or IN TRANSIT (moving between two places). It cannot protect \
                 data that is actively being USED on a device that has already been compromised by \
                 other means — if malware is already running as you, with the vault unlocked and \
                 the plaintext visible on screen, it can read exactly what you can see, the same as \
                 it could with any other program. Being offline does not fix that specific problem, \
                 but it removes an entire, very common CATEGORY of problem: a server this data could \
                 be stolen from, a cloud account that could be breached, a company that could be \
                 hacked or subpoenaed, and an automatic update mechanism that could ever be hijacked \
                 to push malicious code onto your machine. Every one of those exists in a typical \
                 cloud-based password manager. None of them exist here, because there is no server, \
                 no account, and no update channel to attack in the first place — there is only the \
                 file sitting on your own disk.",
            ),
            Block::P(
                "Put simply: the vault is exactly as trustworthy as the device you choose to unlock \
                 it on, and no more. Being offline is what guarantees that statement stays true — it \
                 removes every path by which trust in this program could ever depend on trusting \
                 someone else's server as well.",
            ),
        ],
    },
];

// --- Search ------------------------------------------------------------------

/// The searchable text of a topic: its title, blurb, section, and every word of
/// its body. Built on demand so the content table stays plain data.
fn haystack(t: &Topic) -> String {
    let mut s = String::with_capacity(512);
    s.push_str(t.title);
    s.push(' ');
    s.push_str(t.blurb);
    s.push(' ');
    s.push_str(t.section);
    for b in t.body {
        s.push(' ');
        match b {
            Block::P(p) | Block::Sub(p) | Block::Note(p) | Block::Warn(p) => s.push_str(p),
            Block::Bullets(items) | Block::Steps(items) => {
                for i in *items {
                    s.push_str(i);
                    s.push(' ');
                }
            }
            Block::Rows(rows) => {
                for (k, v) in *rows {
                    s.push_str(k);
                    s.push(' ');
                    s.push_str(v);
                    s.push(' ');
                }
            }
        }
    }
    s.to_lowercase()
}

/// Indices of the topics matching `query`, in manual order. Every whitespace-separated
/// word of the query must appear somewhere in the topic (AND semantics), which is what
/// makes a two-word query like "export documents" narrow rather than widen the result.
/// An empty/whitespace query matches everything.
///
/// A pure function of its input — unit-tested without a UI.
pub(crate) fn search(query: &str) -> Vec<usize> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return (0..TOPICS.len()).collect();
    }
    let words: Vec<&str> = q.split_whitespace().collect();
    TOPICS
        .iter()
        .enumerate()
        .filter(|(_, t)| {
            let hay = haystack(t);
            words.iter().all(|w| hay.contains(w))
        })
        .map(|(i, _)| i)
        .collect()
}

// --- Rendering ---------------------------------------------------------------

/// The mutable state the help browser keeps between frames. Lives in the app so
/// the user's place in the manual survives leaving and re-opening Help.
/// The default lands on the first topic with an empty search box — i.e. the
/// overview, which is what Help should open on.
#[derive(Default)]
pub(crate) struct HelpState {
    /// The search box's contents.
    pub(crate) query: String,
    /// Index into [`TOPICS`] of the article being shown.
    pub(crate) topic: usize,
}

/// The few live facts the manual shows alongside its static text.
pub(crate) struct HelpContext {
    /// This vault's path on disk.
    pub(crate) vault: String,
    /// The non-secret preferences file's path.
    pub(crate) prefs: String,
    /// Whether this session can make changes (shown as the mode badge).
    pub(crate) writable: bool,
}

/// Draw the help browser. Returns `true` when the user asked to go back.
///
/// Takes the accent color rather than reaching for the theme, so this module
/// stays independent of the theme enum in `gui`.
pub(crate) fn ui(ui: &mut egui::Ui, state: &mut HelpState, ctx: &HelpContext, accent: egui::Color32) -> bool {
    let mut back = false;

    // Header: title, mode badge, search, and the way out.
    egui::Panel::top("help_header").show_inside(ui, |ui| {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("⬅ Back").clicked() {
                back = true;
            }
            ui.add_space(4.0);
            ui.heading(egui::RichText::new("Help").color(accent));
            ui.label(
                egui::RichText::new(if ctx.writable { "write mode" } else { "read-only mode" })
                    .weak()
                    .small(),
            );
            // Right-align the search box so it keeps its place as the window resizes.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !state.query.is_empty() && ui.button("×").on_hover_text("Clear the search").clicked() {
                    state.query.clear();
                }
                ui.add(
                    egui::TextEdit::singleline(&mut state.query)
                        .hint_text("🔍  Search the manual…")
                        .desired_width(260.0_f32.min(ui.available_width() - 8.0).max(90.0)),
                );
            });
        });
        ui.add_space(6.0);
    });

    // Left: the index, grouped by section and filtered by the search box.
    let hits = search(&state.query);
    egui::Panel::left("help_nav").resizable(true).default_size(238.0).show_inside(ui, |ui| {
        ui.add_space(6.0);
        if hits.is_empty() {
            ui.label(egui::RichText::new("No topic matches that search.").weak().italics());
            return;
        }
        if !state.query.trim().is_empty() {
            ui.label(egui::RichText::new(format!("{} matching topic(s)", hits.len())).weak().small());
            ui.add_space(4.0);
        }
        egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("help_nav_scroll").show(ui, |ui| {
            for section in SECTIONS {
                // Only draw a section header when the filter left something under it.
                let in_section: Vec<usize> =
                    hits.iter().copied().filter(|i| TOPICS[*i].section == *section).collect();
                if in_section.is_empty() {
                    continue;
                }
                ui.add_space(6.0);
                ui.label(egui::RichText::new(*section).strong().color(accent).small());
                for i in in_section {
                    if ui.selectable_label(state.topic == i, TOPICS[i].title).clicked() {
                        state.topic = i;
                    }
                }
            }
            ui.add_space(8.0);
        });
    });

    // Right: the selected article. If a search hid the current topic, show the
    // first hit instead of an article the index no longer lists.
    if !hits.is_empty() && !hits.contains(&state.topic) {
        state.topic = hits[0];
    }
    egui::CentralPanel::default().show_inside(ui, |ui| {
        let Some(topic) = TOPICS.get(state.topic) else { return };
        egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("help_body_scroll").show(ui, |ui| {
            ui.add_space(6.0);
            ui.label(egui::RichText::new(topic.section).weak().small());
            ui.heading(topic.title);
            ui.label(egui::RichText::new(topic.blurb).italics().weak());
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(6.0);
            for block in topic.body {
                render_block(ui, block, accent);
            }
            // The live paths belong with the article about files, not on every page.
            if topic.id == "overview" {
                ui.add_space(10.0);
                paths_card(ui, ctx);
            }
            ui.add_space(24.0);
        });
    });

    back
}

/// Add a label that wraps at the pane's right edge.
///
/// egui only wraps text by DEFAULT in a vertical layout: inside a horizontal one
/// (a bullet's glyph + its text, a callout's icon + its text) `Ui::wrap_mode`
/// returns `Extend`, so the line runs off the window and the reader has to widen
/// the window to finish the sentence. Asking for `Wrap` explicitly makes the text
/// reflow to whatever width the window currently offers.
fn wrapped(ui: &mut egui::Ui, text: egui::RichText) -> egui::Response {
    ui.add(egui::Label::new(text).wrap_mode(egui::TextWrapMode::Wrap))
}

/// The per-column cap that makes a [`egui::Grid`]'s cells wrap.
///
/// A grid also defaults to `Extend` — it wraps only once a finite max column width
/// is set. 70% of the pane lets the wide right-hand column use most of the width
/// while still bounding the left one, and because the last column additionally gets
/// only the space its neighbour left over, no pair of cells can overflow the pane.
/// The floor keeps the columns legible in a very narrow window.
fn wrap_width(ui: &egui::Ui) -> f32 {
    (ui.available_width() * 0.7).max(160.0)
}

/// Render one content block. All of the manual's typography lives here.
fn render_block(ui: &mut egui::Ui, block: &Block, accent: egui::Color32) {
    match block {
        Block::P(text) => {
            ui.label(*text);
            ui.add_space(8.0);
        }
        Block::Sub(text) => {
            ui.add_space(6.0);
            ui.label(egui::RichText::new(*text).strong().size(15.5).color(accent));
            ui.add_space(4.0);
        }
        Block::Bullets(items) => {
            for item in *items {
                ui.horizontal_top(|ui| {
                    ui.label(egui::RichText::new("•").color(accent).strong());
                    wrapped(ui, egui::RichText::new(*item));
                });
                ui.add_space(3.0);
            }
            ui.add_space(6.0);
        }
        Block::Steps(items) => {
            for (n, item) in items.iter().enumerate() {
                ui.horizontal_top(|ui| {
                    ui.label(egui::RichText::new(format!("{}.", n + 1)).color(accent).strong());
                    wrapped(ui, egui::RichText::new(*item));
                });
                ui.add_space(3.0);
            }
            ui.add_space(6.0);
        }
        Block::Rows(rows) => {
            // A striped two-column grid; the left column is the thing being named
            // (a button, a command, a symptom), the right column what it means.
            egui::Grid::new(rows.as_ptr() as usize) // stable per-table id
                .num_columns(2)
                .striped(true)
                .spacing([16.0, 8.0])
                .max_col_width(wrap_width(ui))
                .show(ui, |ui| {
                    for (k, v) in *rows {
                        ui.label(egui::RichText::new(*k).strong());
                        ui.label(*v);
                        ui.end_row();
                    }
                });
            ui.add_space(10.0);
        }
        Block::Note(text) => {
            callout(ui, "ℹ", text, accent);
        }
        Block::Warn(text) => {
            callout(ui, "⚠", text, egui::Color32::from_rgb(200, 90, 20));
        }
    }
}

/// A tinted, left-barred aside used for both notes and warnings.
fn callout(ui: &mut egui::Ui, glyph: &str, text: &str, color: egui::Color32) {
    egui::Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .stroke(egui::Stroke::new(1.0_f32, color.gamma_multiply(0.55)))
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.label(egui::RichText::new(glyph).color(color).strong());
                wrapped(ui, egui::RichText::new(text));
            });
        });
    ui.add_space(10.0);
}

/// The "where things are on this machine" card shown at the end of the overview.
fn paths_card(ui: &mut egui::Ui, ctx: &HelpContext) {
    egui::Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .corner_radius(6)
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.label(egui::RichText::new("Where things are").strong());
            ui.add_space(6.0);
            // Vault/prefs paths are long; without a cap the card runs off the window.
            let cap = wrap_width(ui);
            egui::Grid::new("help_paths").num_columns(2).spacing([14.0, 6.0]).max_col_width(cap).show(ui, |ui| {
                ui.label("This vault");
                ui.label(egui::RichText::new(&ctx.vault).monospace());
                ui.end_row();
                ui.label("Preferences");
                ui.label(egui::RichText::new(&ctx.prefs).monospace());
                ui.end_row();
            });
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(
                    "prefs.conf holds only the theme, interface size, typeface and the two \
                     grouping defaults. It sits in your vaults folder, is created only when you \
                     change one of those settings, and contains no vault data and no secrets.",
                )
                .weak()
                .small(),
            );
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every block kind, rendered in a deliberately NARROW pane, must stay inside it.
    ///
    /// The bug this pins: egui wraps text by default only in a vertical layout, so
    /// the manual's bullets, numbered steps, callouts and two-column tables — all of
    /// which draw their text in a horizontal layout or a grid — silently defaulted to
    /// `TextWrapMode::Extend` and ran off the right edge of the window. The reader had
    /// to widen the window to see the end of a sentence. Asserting on the drawn width
    /// (rather than eyeballing a screenshot) catches a regression in any one block kind.
    #[test]
    fn every_block_kind_wraps_inside_a_narrow_pane() {
        use egui_kittest::Harness;
        use std::cell::Cell;

        const LONG: &str = "This sentence is deliberately far longer than any narrow \
                            window could ever show on a single line, so if it does not \
                            wrap it will run off the right-hand edge of the pane.";
        const ROWS: &[(&str, &str)] = &[("A key", LONG), ("Another key", LONG)];
        const PANE: f32 = 320.0;

        for block in [
            Block::P(LONG),
            Block::Sub(LONG),
            Block::Bullets(&[LONG, LONG]),
            Block::Steps(&[LONG, LONG]),
            Block::Rows(ROWS),
            Block::Note(LONG),
            Block::Warn(LONG),
        ] {
            let drawn = Cell::new(0.0f32);
            let mut h = Harness::new_ui(|ui| {
                ui.set_max_width(PANE);
                render_block(ui, &block, egui::Color32::from_rgb(0, 120, 200));
                drawn.set(ui.min_rect().width());
            });
            // Two frames: a grid only knows its column widths from the previous frame.
            h.run();
            h.run();
            assert!(
                drawn.get() <= PANE + 1.0,
                "{block:?} drew {}pt wide in a {PANE}pt pane — it is not wrapping",
                drawn.get()
            );
        }
    }

    #[test]
    fn every_topic_is_well_formed_and_uniquely_identified() {
        let mut ids: Vec<&str> = Vec::new();
        for t in TOPICS {
            assert!(!t.title.is_empty(), "topic {} has no title", t.id);
            assert!(!t.blurb.is_empty(), "topic {} has no blurb", t.id);
            assert!(!t.body.is_empty(), "topic {} has an empty body", t.id);
            assert!(
                SECTIONS.contains(&t.section),
                "topic {} is filed under unknown section {:?}",
                t.id,
                t.section
            );
            assert!(!ids.contains(&t.id), "duplicate topic id {}", t.id);
            ids.push(t.id);
        }
        // Every declared section must actually have topics, or the index would
        // render a heading with nothing under it.
        for s in SECTIONS {
            assert!(TOPICS.iter().any(|t| t.section == *s), "section {s:?} has no topics");
        }
    }

    #[test]
    fn search_matches_title_body_and_requires_every_word() {
        // Empty query = the whole manual.
        assert_eq!(search("").len(), TOPICS.len());
        assert_eq!(search("   ").len(), TOPICS.len());

        // Matches on the title, case-insensitively.
        let hits = search("URGENT");
        assert!(hits.iter().any(|i| TOPICS[*i].id == "tab-urgent"));

        // Matches on body text, not just titles: "Argon2id" appears only in prose.
        let hits = search("argon2id");
        assert!(!hits.is_empty(), "a word that appears only in body text must be findable");
        assert!(hits.iter().any(|i| TOPICS[*i].id == "passwords" || TOPICS[*i].id == "security"));

        // AND semantics: adding a word narrows the result rather than widening it.
        let one = search("export").len();
        let two = search("export clipboard").len();
        assert!(two <= one, "a second word must not widen the result set ({two} > {one})");

        // A word in no article matches nothing.
        assert!(search("zzzznotaword").is_empty());
    }

    /// Each feature the manual is supposed to explain must be REACHABLE by the words a user
    /// would type into the help search — a topic nobody can find is not documentation. The
    /// query/topic pairs below are the behaviours that are not self-evident from the screen.
    #[test]
    fn every_documented_feature_is_findable_by_search() {
        for (query, expect_id) in [
            // Quoted / pasted paths, in the dedicated topic and where they are used.
            ("quoted path", "paths"),
            ("copy as path", "paths"),
            // The search box: how it looks and how it matches.
            ("sound-alike", "searching"),
            ("search box", "searching"),
            // The searchable link dropdown.
            ("link an account dropdown", "links"),
            // The sample vault the build script produces.
            ("sample vault", "demo-vault"),
            ("sample1", "demo-vault"),
            // Long-standing behaviour that is easy to miss.
            ("trim all fields", "editing"),
            ("before tax bucket", "tab-summary"),
        ] {
            let hits = search(query);
            assert!(
                hits.iter().any(|i| TOPICS[*i].id == expect_id),
                "searching the manual for {query:?} must reach the {expect_id:?} topic; \
                 it matched {:?}",
                hits.iter().map(|i| TOPICS[*i].id).collect::<Vec<_>>()
            );
        }
    }

    /// Every cross-reference — the manual's `See “Article title”` / `see “Article title”` form —
    /// must name a topic that actually exists, so following one never sends the reader hunting for
    /// an article that was renamed or never written. Only quoted phrases introduced by "see" are
    /// checked; other quoted text is button labels and values, which are not references.
    #[test]
    fn cross_references_name_real_topics() {
        // `haystack` lower-cases (it feeds the search), so compare lower-cased titles.
        let titles: Vec<String> = TOPICS.iter().map(|t| t.title.to_lowercase()).collect();
        let mut checked = 0usize;
        for t in TOPICS {
            let text = haystack(t);
            // Scan for the marker itself rather than pairing every quote in the article: the
            // prose quotes UI labels too, and pairing sequentially misaligns on those.
            for marker in ["see \u{201c}"] {
                let mut rest = text.as_str();
                while let Some(at) = rest.find(marker) {
                    rest = &rest[at + marker.len()..];
                    let Some(close) = rest.find('\u{201d}') else {
                        panic!("topic {} has an unterminated cross-reference", t.id)
                    };
                    let referenced = &rest[..close];
                    rest = &rest[close..];
                    checked += 1;
                    assert!(
                        titles.iter().any(|title| title == referenced),
                        "topic {} points at {referenced:?}, which is not an article title",
                        t.id
                    );
                }
            }
        }
        // Guard against the check silently becoming vacuous if the convention changes.
        assert!(checked >= 8, "only {checked} cross-references found — has the “See …” convention changed?");
    }

    #[test]
    fn troubleshooting_covers_the_failure_users_actually_hit() {
        // The read-only surprise is the single most common confusion, so the manual
        // must always answer it somewhere findable.
        let hits = search("read-only");
        assert!(hits.len() >= 2, "read-only must be documented in more than one place");
        assert!(!search("password rejected").is_empty() || !search("passwords are rejected").is_empty());
    }
}
