//! Where each category lives inside an Instagram export. As with Facebook,
//! paths drift between vintages (`content/posts_1.json` before 2024,
//! `your_instagram_activity/content/posts_1.json` since, often under a
//! wrapper directory), so every match is on the file name and its parent
//! segments — never an exact path. A file that matches nothing is ignored
//! (§ Parser contract rule 2): stories, saved items, ads, login history.

use std::collections::HashMap;

use crate::facebook::layout::{
    ThreadFiles, file_name, file_thread_member, has_segment, is_message_file, parent, sort_natural,
    sort_threads,
};
use crate::model::Category;
use crate::reader::ZipDirectory;

/// The located members, per category, each list in natural order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Layout {
    /// An Instagram-signature `.json` member exists.
    pub json: bool,
    /// An Instagram-signature `.html` member exists.
    pub html: bool,
    pub profile: Option<String>,
    /// Feed posts (`posts_N.json`), reels and archived posts.
    pub posts: Vec<String>,
    pub comments: Vec<String>,
    pub reactions: Vec<String>,
    pub followers: Vec<String>,
    pub following: Vec<String>,
    pub threads: Vec<ThreadFiles>,
}

const PROFILE_SUFFIX: &str = "personal_information/personal_information/personal_information";

/// `posts_<digits>` — the feed-post members (`posts_1.json`, `posts_2.json`, …).
fn is_numbered(stem: &str, prefix: &str) -> bool {
    stem.strip_prefix(prefix)
        .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
}

fn stem(file: &str) -> &str {
    file.rsplit_once('.').map_or(file, |(s, _)| s)
}

/// A member path that only an Instagram export produces.
pub fn is_signature(lower: &str) -> bool {
    has_segment(lower, "your_instagram_activity")
        || lower.ends_with(&format!("{PROFILE_SUFFIX}.json"))
        || lower.ends_with(&format!("{PROFILE_SUFFIX}.html"))
        || (parent(lower) == Some("content") && is_numbered(stem(file_name(lower)), "posts_"))
}

/// Whether `file` (in a `content` or `media` directory) holds own posts.
fn is_post_member(file: &str) -> bool {
    is_numbered(stem(file), "posts_") || matches!(file, "reels.json" | "archived_posts.json")
}

/// Walks the directory once and files every recognised member.
pub fn locate(dir: &ZipDirectory) -> Layout {
    let mut layout = Layout::default();
    let mut thread_at: HashMap<String, usize> = HashMap::new();
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
        if lower.ends_with(&format!("{PROFILE_SUFFIX}.json")) {
            layout.profile = Some(owned);
        } else if matches!(parent, Some("content" | "media")) && is_post_member(file) {
            layout.posts.push(owned);
        } else if parent == Some("comments")
            && (file.starts_with("post_comments") || file == "reels_comments.json")
        {
            layout.comments.push(owned);
        } else if parent == Some("likes")
            && matches!(file, "liked_posts.json" | "liked_comments.json")
        {
            layout.reactions.push(owned);
        } else if parent == Some("followers_and_following") && file.starts_with("followers_") {
            layout.followers.push(owned);
        } else if parent == Some("followers_and_following") && file == "following.json" {
            layout.following.push(owned);
        } else if has_segment(&lower, "messages") && is_message_file(file) {
            file_thread_member(&mut layout.threads, &mut thread_at, owned);
        }
    }
    for list in [
        &mut layout.posts,
        &mut layout.comments,
        &mut layout.reactions,
        &mut layout.followers,
        &mut layout.following,
    ] {
        sort_natural(list);
    }
    sort_threads(&mut layout.threads);
    layout
}

impl Layout {
    /// The members a category is read from, in order. Instagram has no
    /// albums, events or groups: those categories are read from nothing and
    /// count zero (§ Parser contract rule 2).
    pub fn members_for(&self, category: &Category) -> Vec<String> {
        match category {
            Category::Posts => self.posts.clone(),
            Category::Comments => self.comments.clone(),
            Category::Reactions => self.reactions.clone(),
            Category::Friends => {
                let mut all = self.followers.clone();
                all.extend(self.following.iter().cloned());
                all
            }
            Category::Threads | Category::Messages => self
                .threads
                .iter()
                .flat_map(|t| t.files.iter().cloned())
                .collect(),
            Category::Profile => self.profile.iter().cloned().collect(),
            Category::Albums | Category::Events | Category::Groups | Category::Other(_) => {
                Vec::new()
            }
        }
    }

    /// Every recognised member, for "what did the parser ignore" checks.
    pub fn all_members(&self) -> impl Iterator<Item = &String> {
        self.profile
            .iter()
            .chain(&self.posts)
            .chain(&self.comments)
            .chain(&self.reactions)
            .chain(&self.followers)
            .chain(&self.following)
            .chain(self.threads.iter().flat_map(|t| t.files.iter()))
    }
}
