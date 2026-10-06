//! Where each category lives inside a Facebook export. Paths drift between
//! export vintages (`posts/your_posts_1.json` in 2019, `your_facebook_activity/
//! posts/your_posts__check_ins__photos_and_videos_1.json` since 2023, often
//! under a wrapper directory), so every match is on the file name and its
//! parent segments — never an exact path. A file that matches nothing is
//! ignored (§ Parser contract rule 2).

use crate::model::Category;
use crate::reader::ZipDirectory;

/// Extensions the index sizes as media (never read during `index`).
pub const MEDIA_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "heic", "mp4", "mov", "m4v", "webm", "mp3", "m4a", "aac",
    "wav",
];

/// One message thread: its directory and its `message_N.json` files in
/// natural order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadFiles {
    pub dir: String,
    pub files: Vec<String>,
}

/// The located members, per category, each list in natural (`_1`, `_2`,
/// `_10`) order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Layout {
    /// A Facebook-signature `.json` member exists.
    pub json: bool,
    /// A Facebook-signature `.html` member exists.
    pub html: bool,
    pub profile: Option<String>,
    pub posts: Vec<String>,
    pub albums: Vec<String>,
    pub comments: Vec<String>,
    pub reactions: Vec<String>,
    pub friends: Vec<String>,
    pub followers: Vec<String>,
    pub following: Vec<String>,
    pub events: Vec<String>,
    pub groups: Vec<String>,
    pub threads: Vec<ThreadFiles>,
}

fn file_name(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

fn parent(name: &str) -> Option<&str> {
    let mut it = name.rsplit('/');
    it.next()?;
    it.next()
}

fn grandparent(name: &str) -> Option<&str> {
    let mut it = name.rsplit('/');
    it.next()?;
    it.next()?;
    it.next()
}

fn has_segment(name: &str, segment: &str) -> bool {
    name.split('/').any(|s| s == segment)
}

fn extension(name: &str) -> &str {
    file_name(name).rsplit('.').next().unwrap_or("")
}

/// A member path that only a Facebook export produces.
pub fn is_signature(lower: &str) -> bool {
    has_segment(lower, "your_facebook_activity")
        || lower.ends_with("profile_information/profile_information.json")
        || lower.ends_with("profile_information/profile_information.html")
        || (parent(lower) == Some("posts") && file_name(lower).starts_with("your_posts"))
}

fn is_message_file(file: &str) -> bool {
    file.strip_prefix("message_")
        .and_then(|rest| rest.strip_suffix(".json"))
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

/// Sort key that orders `_1`, `_2`, `_10` numerically: (stem without the
/// trailing digit run, that run as a number).
pub fn natural_key(name: &str) -> (String, u64) {
    let stem = match name.rfind('.') {
        Some(dot) if dot > name.rfind('/').map_or(0, |s| s + 1) => &name[..dot],
        _ => name,
    };
    let digits_start = stem
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_ascii_digit())
        .last()
        .map(|(i, _)| i)
        .unwrap_or(stem.len());
    let number = stem[digits_start..].parse::<u64>().unwrap_or(0);
    (stem[..digits_start].to_string(), number)
}

fn sort_natural(list: &mut [String]) {
    list.sort_by_cached_key(|n| natural_key(n));
}

/// Walks the directory once and files every recognised member.
///
/// Linear in entries. The thread grouping used to be a `threads.iter().find()`
/// inside this loop, which is O(entries × threads) *by construction* — and
/// `locate` is re-derived on every `detect` / `index` / `stream` call, roughly a
/// dozen times per import and twice at file-pick, over a directory listing the
/// archive itself supplies. `thread_at` keeps the O(1) lookup while `threads`
/// stays an ordered `Vec`, so the layout's output is byte-identical to before.
pub fn locate(dir: &ZipDirectory) -> Layout {
    let mut layout = Layout::default();
    let mut thread_at: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for entry in dir.entries() {
        let name = entry.name.as_str();
        let lower = name.to_ascii_lowercase();
        let file = file_name(&lower);
        let parent = parent(&lower);
        if is_signature(&lower) {
            if lower.ends_with(".json") {
                layout.json = true;
            } else if lower.ends_with(".html") {
                layout.html = true;
            }
        }
        if !lower.ends_with(".json") {
            continue;
        }
        let owned = name.to_string();
        if lower.ends_with("profile_information/profile_information.json") {
            layout.profile = Some(owned);
        } else if parent == Some("posts") && file.starts_with("your_posts") {
            layout.posts.push(owned);
        } else if parent == Some("album") && grandparent(&lower) == Some("posts") {
            layout.albums.push(owned);
        } else if file == "comments.json"
            && matches!(parent, Some("comments_and_reactions" | "comments"))
        {
            layout.comments.push(owned);
        } else if (file.starts_with("likes_and_reactions")
            && parent == Some("comments_and_reactions"))
            || (file == "posts_and_comments.json" && parent == Some("likes_and_reactions"))
        {
            layout.reactions.push(owned);
        } else if matches!(file, "your_friends.json" | "friends.json") && parent == Some("friends")
        {
            layout.friends.push(owned);
        } else if parent == Some("followers_and_following") && file.starts_with("followers") {
            layout.followers.push(owned);
        } else if parent == Some("followers_and_following") && file.starts_with("who_you_follow") {
            layout.following.push(owned);
        } else if matches!(
            file,
            "your_event_responses.json" | "event_invitations.json" | "your_events.json"
        ) && parent == Some("events")
        {
            layout.events.push(owned);
        } else if file == "your_group_membership_activity.json" && parent == Some("groups") {
            layout.groups.push(owned);
        } else if has_segment(&lower, "messages") && is_message_file(file) {
            let dir_name = name[..name.rfind('/').unwrap_or(0)].to_string();
            match thread_at.get(&dir_name) {
                Some(&i) => layout.threads[i].files.push(owned),
                None => {
                    thread_at.insert(dir_name.clone(), layout.threads.len());
                    layout.threads.push(ThreadFiles {
                        dir: dir_name,
                        files: vec![owned],
                    });
                }
            }
        }
    }
    for list in [
        &mut layout.posts,
        &mut layout.albums,
        &mut layout.comments,
        &mut layout.reactions,
        &mut layout.friends,
        &mut layout.followers,
        &mut layout.following,
        &mut layout.events,
        &mut layout.groups,
    ] {
        sort_natural(list);
    }
    layout.threads.sort_by(|a, b| a.dir.cmp(&b.dir));
    for thread in &mut layout.threads {
        sort_natural(&mut thread.files);
    }
    layout
}

impl Layout {
    /// The members a category is read from, in order. A category this build
    /// cannot name is read from nothing.
    pub fn members_for(&self, category: &Category) -> Vec<String> {
        match category {
            Category::Posts => self.posts.clone(),
            Category::Albums => self.albums.clone(),
            Category::Comments => self.comments.clone(),
            Category::Reactions => self.reactions.clone(),
            Category::Events => self.events.clone(),
            Category::Groups => self.groups.clone(),
            Category::Friends => {
                let mut all = self.friends.clone();
                all.extend(self.followers.iter().cloned());
                all.extend(self.following.iter().cloned());
                all
            }
            Category::Threads | Category::Messages => self
                .threads
                .iter()
                .flat_map(|t| t.files.iter().cloned())
                .collect(),
            Category::Profile => self.profile.iter().cloned().collect(),
            Category::Other(_) => Vec::new(),
        }
    }

    /// Every recognised member, for "what did the parser ignore" checks.
    pub fn all_members(&self) -> impl Iterator<Item = &String> {
        self.profile
            .iter()
            .chain(&self.posts)
            .chain(&self.albums)
            .chain(&self.comments)
            .chain(&self.reactions)
            .chain(&self.friends)
            .chain(&self.followers)
            .chain(&self.following)
            .chain(&self.events)
            .chain(&self.groups)
            .chain(self.threads.iter().flat_map(|t| t.files.iter()))
    }

    /// Bytes of every media-extension member — sized from the directory.
    pub fn media_bytes(&self, dir: &ZipDirectory) -> u64 {
        dir.entries()
            .iter()
            .filter(|e| {
                let ext = extension(&e.name).to_ascii_lowercase();
                MEDIA_EXTENSIONS.contains(&ext.as_str())
            })
            .map(|e| e.size)
            .sum()
    }
}
