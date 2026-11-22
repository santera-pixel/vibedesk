//! The skill plane: what the swarm knows how to do, and who is told to do it.
//!
//! A swarm member starts with no memory of this repository and no method. It
//! reads the brief, writes code, and commits. The skills installed on this
//! machine are the accumulated method — how to build a 3D scene, how to review a
//! change, how to debug a failing test — and a run that never loads one is a run
//! that reinvents whatever it needed, badly, at fifteen times the token cost of a
//! chat.
//!
//! So this module is not a hint appended to a prompt. It is four decisions:
//!
//!   rank      which installed skills bear on this goal, and why
//!   assign    which skill each task gets, from the plan or from the ranking
//!   render    the exact instruction that makes a member load one
//!   observe   whether it actually did
//!
//! The last one is what makes the rest honest. A brief that says "consider the
//! skills" is unverifiable, and an unverifiable instruction is decoration. The
//! engine reads the member's own tool calls, so "loaded" is a fact about the run
//! rather than a claim in a transcript.
//!
//! ## Why the ranking is not a model call
//!
//! It could be — one cheap turn over the catalogue and the goal. It is not,
//! because the ranking runs before the plan, on every run, and a model call there
//! would add a failure mode to the one step that must never fail: the step that
//! decides whether the run starts at all. Token overlap with inverse document
//! frequency is deterministic, costs nothing, and can be explained to the user in
//! the panel ("matched: 3d, scene, three"). A ranking nobody can inspect is a
//! ranking nobody can correct.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::skills as catalog;

/// One installed skill, as the swarm needs it.
///
/// `path` is the load-bearing field. A member's session is opened with
/// `mcpServers: []`, so the mastery bundles — which arrive only through the
/// `mcp_mastery` tool — are not reachable from inside a run. The file is. Every
/// skill is therefore loadable by reading its `SKILL.md`, and the brief says so
/// rather than naming a tool the member does not have.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// The `SKILL.md` this skill is defined by.
    pub path: String,
    /// `project`, `user`, `plugin` or `mastery`.
    pub scope: String,
    /// The plugin or mastery bundle it came from, when it came from one.
    pub bundle: Option<String>,
    pub enabled: bool,
    /// The opening of the skill's own file, for ranking only.
    ///
    /// Not serialized. A run document carries the catalogue's metadata — what is
    /// installed, where it lives, whether it is on — and a body is a thing to
    /// read from disk when it is needed rather than a thing to copy into every
    /// run. It is here because a one-line description is not enough text to rank
    /// against: `3dviz-pro-max` describes itself as "3D scenes and interactive
    /// models" and its body is the only place that says `three.js`, `geometry`,
    /// `camera` and `renderer`, which is the vocabulary a goal about a game
    /// actually uses.
    #[serde(skip)]
    pub body: String,
}

/// The scope of a skill a run wrote for itself.
///
/// It lives in the run's own directory, is handed to that run's members, and is
/// gone when the run is deleted. It is not installed: a run that could write to
/// `~/.vibe/skills` could change how every future run behaves, and that is not a
/// side effect to take on trust. Keeping one is a click, through the same
/// proposal path the reflection pass uses.
pub const RUN_SCOPE: &str = "run";

impl Skill {
    /// Whether the CLI's own skill tool can load this one by name.
    ///
    /// A mastery bundle is not in the CLI's catalogue — it is a cloned upstream
    /// repo the mastery store enabled, and the CLI only learns about it through
    /// the mastery MCP server, which a member session does not have. A run-scoped
    /// skill is not in it either: it was written after the session started, and
    /// the CLI's discovery ran before that. Naming `skill { name }` for either
    /// would be an instruction that fails, so both are loadable by path only.
    pub fn tool_loadable(&self) -> bool {
        matches!(self.scope.as_str(), "project" | "user" | "plugin")
    }

    /// The instruction that loads this skill, in the member's own terms.
    pub fn load_instruction(&self) -> String {
        if self.tool_loadable() {
            format!(
                "call `skill {{ name: \"{}\" }}`, or read `{}` and follow it",
                self.name, self.path
            )
        } else {
            format!("read `{}` and follow it", self.path)
        }
    }
}

/// Every skill the agent can see, in the order the CLI resolves them.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalogue {
    pub skills: Vec<Skill>,
}

impl Catalogue {
    /// Read the catalogue from disk.
    ///
    /// The same discovery the skills panel uses, so the panel and the run cannot
    /// disagree about what is installed. A missing directory is an empty
    /// catalogue, not an error: a machine with no skills is a machine where the
    /// swarm runs without them, which is what it did before this existed.
    pub fn load(cwd: Option<&Path>) -> Self {
        let records = catalog::discover_all(cwd);
        Self {
            skills: records
                .into_iter()
                .map(|record| Skill {
                    name: record.name,
                    description: record.description,
                    body: read_body(&record.source),
                    path: record.source,
                    scope: record.scope,
                    bundle: record.plugin,
                    enabled: record.enabled,
                })
                .collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.iter().find(|skill| skill.name == name)
    }

    /// The names, for a refusal message that tells the lead what it may name.
    pub fn names(&self) -> Vec<String> {
        self.skills.iter().map(|skill| skill.name.clone()).collect()
    }

    /// Only the skills a run may assign: enabled ones.
    ///
    /// A disabled skill is installed and switched off. Assigning it would put an
    /// instruction in a brief that the agent is configured to ignore.
    pub fn assignable(&self) -> impl Iterator<Item = &Skill> {
        self.skills.iter().filter(|skill| skill.enabled)
    }
}

/// How much of a skill's file is read for ranking.
///
/// The opening of a `SKILL.md` says what the skill is about; the rest is the
/// procedure. Ranking wants the first part and would only be diluted by the
/// second, so the body is capped rather than read whole. The cap is generous
/// enough to cover the "when to use this" section that every well-written skill
/// opens with.
const BODY_CHARS: usize = 2400;

/// The opening of a skill's file, without its frontmatter.
///
/// A missing or unreadable file is an empty body, not an error: the skill is
/// still installed and still assignable, it just ranks on its description alone.
fn read_body(path: &str) -> String {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return String::new();
    };
    let text = strip_frontmatter(&raw);
    text.chars().take(BODY_CHARS).collect()
}

/// Drop a leading `---` block, which is metadata rather than prose.
fn strip_frontmatter(raw: &str) -> &str {
    let trimmed = raw.trim_start();
    let Some(rest) = trimmed.strip_prefix("---") else {
        return trimmed;
    };
    match rest.find("\n---") {
        Some(end) => rest[end + 4..].trim_start(),
        None => trimmed,
    }
}

/* ------------------------------------------------------------------ *
 * Ranking
 * ------------------------------------------------------------------ */

/// One skill's relevance to a piece of text, with the reason.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Match {
    pub name: String,
    pub score: f64,
    /// The terms that matched, so the panel can say why rather than assert.
    pub why: Vec<String>,
}

/// Words that appear in every goal and every description, and so distinguish
/// nothing. Kept short on purpose: a long stopword list is a list of words
/// somebody's domain needed.
///
/// The function words are the exception, and they are here because leaving them
/// out is not a matter of taste. `to` is two characters, so it survived the
/// length floor, and it appears in almost every skill description — which made
/// it the single highest-scoring term for a goal about a minimap, and put six
/// arbitrary skills at the top of the shortlist. A word that carries no meaning
/// cannot be evidence, however rare it looks.
const STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "from", "into", "when", "then", "than", "them",
    "they", "there", "these", "those", "have", "has", "had", "was", "were", "are", "will", "would",
    "can", "could", "should", "may", "might", "must", "not", "but", "you", "your", "our", "its",
    "it's", "use", "used", "using", "make", "makes", "made", "add", "adds", "added", "new", "one",
    "two", "all", "any", "each", "every", "some", "more", "most", "other", "such", "only", "also",
    "how", "what", "which", "who", "why", "where", "about", "over", "under", "after", "before",
    "between", "through", "during", "without", "within", "along", "across", "behind", "beyond",
    "skill", "skills", "task", "tasks", "work", "works", "working", "code", "file", "files",
    "run", "runs", "running", "test", "tests", "testing", "change", "changes", "changing",
    "need", "needs", "needed", "want", "wants", "wanted", "like", "just", "very", "much", "many",
    "same", "different", "first", "last", "next", "then", "now", "here", "where", "while",
    // Function words. Two letters is above the length floor, so these have to be
    // named or they become terms.
    "to", "of", "in", "on", "at", "by", "as", "or", "if", "is", "be", "do", "does", "did", "so",
    "up", "out", "off", "no", "yes", "it", "we", "us", "my", "me", "he", "she", "him", "her",
    "their", "an", "a", "am", "been", "being", "both", "either", "neither", "nor", "per", "via",
    "yet", "still", "even", "too", "own", "same", "get", "gets", "got", "put", "puts", "let",
    "lets", "keep", "keeps", "kept", "see", "sees", "saw", "seen", "say", "says", "said", "go",
    "goes", "went", "gone", "come", "comes", "came", "take", "takes", "took", "taken", "give",
    "gives", "gave", "given", "well", "back", "down", "again", "once", "twice", "always", "never",
    "often", "sometimes", "usually", "rather", "quite", "really", "actually", "simply", "instead",
    "however", "therefore", "because", "since", "unless", "until", "though", "although", "whether",
];

/// Split text into comparable terms.
///
/// Lowercased, split on anything that is not alphanumeric, stopwords dropped,
/// and single characters dropped. `3d` survives because it is two characters and
/// is the whole point of a 3D skill; `a` does not.
fn terms(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.len() >= 2)
        .map(|word| word.to_lowercase())
        .filter(|word| !STOPWORDS.contains(&word.as_str()))
        .collect()
}

/// The three places a skill's vocabulary lives, in order of authority.
///
/// The name is the skill's identity, the description is its own statement of
/// what it is for, and the body is the substance. A hit in each is worth
/// different amounts, and keeping them apart is what lets a body hit be real
/// evidence without letting a body hit outrank a skill that says it is for this
/// in its own one-line description.
#[derive(Debug, Default)]
struct Terms {
    name: BTreeSet<String>,
    description: BTreeSet<String>,
    body: BTreeSet<String>,
}

impl Terms {
    /// Every term, for the document-frequency count.
    fn all(&self) -> impl Iterator<Item = &String> {
        self.name
            .iter()
            .chain(self.description.iter())
            .chain(self.body.iter())
    }

    /// The best tier this term hits, and the term it hit.
    ///
    /// Exact before near, and name before description before body. A prefix hit
    /// is `debug` against `debugging`, `render` against `renderer`: not a
    /// stemmer, because a stemmer is a dictionary and a dictionary is a thing to
    /// maintain and to be wrong about. Four characters is the shortest prefix
    /// that is a word rather than a coincidence.
    fn hit(&self, term: &str) -> Option<(Tier, String)> {
        for tier in [Tier::Name, Tier::Description, Tier::Body] {
            if self.of(tier).contains(term) {
                return Some((tier, term.to_string()));
            }
        }
        for tier in [Tier::Name, Tier::Description, Tier::Body] {
            if let Some(known) = self.of(tier).iter().find(|known| {
                known.len() >= 4
                    && term.len() >= 4
                    && (known.starts_with(term) || term.starts_with(known.as_str()))
            }) {
                return Some((tier, known.clone()));
            }
        }
        None
    }

    fn of(&self, tier: Tier) -> &BTreeSet<String> {
        match tier {
            Tier::Name => &self.name,
            Tier::Description => &self.description,
            Tier::Body => &self.body,
        }
    }
}

/// Where a term was found, and what that is worth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tier {
    Name,
    Description,
    Body,
}

impl Tier {
    /// The multiplier for an exact hit in this tier.
    ///
    /// A name hit is the strongest signal there is. A description hit is the
    /// skill saying what it is for. A body hit is the skill's prose mentioning
    /// the word, which is real evidence and weaker evidence, so it is worth most
    /// of a description hit rather than a token amount: `three.js` appears in
    /// `3dviz-pro-max`'s body and nowhere in its description, and a goal about a
    /// browser game should still find it.
    fn exact(self) -> f64 {
        match self {
            Tier::Name => 3.0,
            Tier::Description => 1.0,
            Tier::Body => 0.8,
        }
    }

    /// A prefix hit is weaker than the whole word, so a goal that says
    /// `debugging` is never beaten by one that says `debug`.
    fn near(self) -> f64 {
        self.exact() * 0.6
    }
}

/// The terms a skill is described by, split into the three tiers.
///
/// The name is split on `-`, `_` and `:` because a skill called `3dviz-pro-max`
/// is about 3D, viz and pro, and a tokeniser that kept it whole would match only
/// a goal that happened to contain the literal string.
fn skill_terms(skill: &Skill) -> Terms {
    let mut out = Terms::default();
    for part in skill
        .name
        .split(|c: char| c == '-' || c == '_' || c == ':' || c == ' ')
    {
        if part.len() >= 2 {
            out.name.insert(part.to_lowercase());
        }
    }
    out.description.extend(terms(&skill.description));
    out.body.extend(terms(&skill.body));
    out
}

/// Rank the catalogue against a piece of text.
///
/// Four signals, in increasing weight:
///
/// - a body term in common, weighted by how rare it is across the whole library;
/// - a description term in common, weighted the same way;
/// - a name term in common, because the name is the skill's identity;
/// - the name written out in full in the text, which is the user or the lead
///   naming the skill on purpose and outranks any inference.
///
/// The rarity is counted over every skill's name, description **and body**, not
/// over the descriptions alone. That is the difference between a ranker that
/// works and one that does not: in a corpus of thirty-one one-line descriptions,
/// `stop` appears once and looks like a rare, meaningful term, so a goal saying
/// "make the HUD stop flickering" ranked the anti-slop skill first. In the same
/// thirty-one skills' bodies, `stop` appears in twenty-four of them — it is an
/// ordinary English word, and the count says so.
///
/// Deterministic, and the returned `why` is the evidence for the score.
pub fn rank(catalogue: &Catalogue, text: &str, limit: usize) -> Vec<Match> {
    let wanted = terms(text);
    if wanted.is_empty() || catalogue.is_empty() {
        return Vec::new();
    }
    let wanted: BTreeSet<String> = wanted.into_iter().collect();
    let lowered = text.to_lowercase();

    // Document frequency per term, over every tier of the assignable catalogue.
    let mut frequency: BTreeMap<String, usize> = BTreeMap::new();
    let described: Vec<(usize, Terms)> = catalogue
        .assignable()
        .enumerate()
        .map(|(index, skill)| {
            let set = skill_terms(skill);
            let mut seen: BTreeSet<&String> = BTreeSet::new();
            for term in set.all() {
                seen.insert(term);
            }
            for term in seen {
                *frequency.entry(term.clone()).or_insert(0) += 1;
            }
            (index, set)
        })
        .collect();

    let total = described.len().max(1) as f64;
    let mut matches: Vec<Match> = Vec::new();

    for (index, set) in described {
        let skill = &catalogue.skills[index];
        let mut score = 0.0;
        let mut why: Vec<String> = Vec::new();

        for term in &wanted {
            let Some((tier, hit)) = set.hit(term) else {
                continue;
            };
            let exact = hit == *term;
            let df = *frequency.get(&hit).unwrap_or(&1) as f64;
            // Inverse document frequency, clamped: a term in every skill still
            // counts a little, a term in one skill counts a lot.
            let weight = (total / df).ln().max(0.15) + 0.15;
            let factor = if exact { tier.exact() } else { tier.near() };
            score += weight * factor;
            why.push(if exact {
                term.clone()
            } else {
                format!("{term}~{hit}")
            });
        }

        // The name written out in full. A user who types `@assay` or a lead that
        // writes `3dviz-pro-max` has already decided; the ranking only has to
        // not lose it.
        let name = skill.name.to_lowercase();
        let named = name.len() >= 3 && lowered.contains(&name);
        if named {
            score += 6.0;
        }

        if score > 0.0 {
            why.sort();
            why.dedup();
            // Inserted after the sort, because the reason a skill was picked is
            // the first thing the panel shows and "named:" outranks a term hit.
            if named {
                why.insert(0, format!("named: {name}"));
            }
            matches.push(Match {
                name: skill.name.clone(),
                score,
                why,
            });
        }
    }

    matches.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.name.cmp(&b.name))
    });
    matches.truncate(limit);
    matches
}

/// The score below which a match is noise rather than a signal.
///
/// One shared rare term is enough to be worth a line in a brief; a shared common
/// term is not. The threshold is on the score rather than on the count of
/// matched terms because the weights already encode rarity.
pub const ASSIGN_THRESHOLD: f64 = 1.0;

/// The skills a task should be given when the plan named none.
///
/// The task's own text, not the run's goal: two tasks in one run are usually
/// about different things, and a task that inherits the goal's skills gets the
/// ones the lead already used.
pub fn assign(catalogue: &Catalogue, task_text: &str, limit: usize) -> Vec<String> {
    rank(catalogue, task_text, limit)
        .into_iter()
        .filter(|entry| entry.score >= ASSIGN_THRESHOLD)
        .map(|entry| entry.name)
        .collect()
}

/// The text a task is ranked by: its title, its detail, and its files.
///
/// The files matter. `src/scene/renderer.ts` says more about which skill a task
/// needs than a title like "wire the renderer" does.
pub fn task_text(title: &str, detail: &str, files: &[String]) -> String {
    let mut text = format!("{title}\n{detail}");
    for file in files {
        text.push('\n');
        text.push_str(file);
    }
    text
}

/* ------------------------------------------------------------------ *
 * Rendering
 * ------------------------------------------------------------------ */

/// The brief section that makes a member load its skills.
///
/// Written as an obligation with a mechanism, not a suggestion. The mechanism is
/// the path, because it is the one thing that works in every member session: the
/// CLI's skill tool is there for project, user and plugin skills, and the file is
/// there for all of them.
pub fn render_for_worker(skills: &[&Skill]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "## Skills you must load before you write anything\n\
         The run selected these for this task. Load each one, then follow it. A skill\n\
         you skip is a method this run paid for and did not use, and the verifier is\n\
         told which ones you were given.\n\n",
    );
    for skill in skills {
        out.push_str(&format!(
            "- `{}` — {}\n  Load it: {}\n",
            skill.name,
            one_line(&skill.description),
            skill.load_instruction()
        ));
    }
    out.push_str(
        "\nIf a skill contradicts this brief, the brief wins: the brief is about this\n\
         task and the skill is about the method. If a skill is wrong about this\n\
         repository, say so in a `decision` entry rather than silently ignoring it.\n",
    );
    out
}

/// The verifier's version: the same skills, plus what the worker was given.
///
/// A verifier that does not know the method cannot judge whether the method was
/// followed, and a verifier that is not told which skills the worker was given
/// cannot notice that none of them were loaded.
pub fn render_for_verifier(skills: &[&Skill], loaded: &[String]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "## The method this task was given\n\
         These skills were assigned to the worker. You are not judging whether it read\n\
         them; you are judging the change. But a change that contradicts the method it\n\
         was handed is a change that will not survive review, and that is worth a\n\
         failing verdict.\n\n",
    );
    for skill in skills {
        out.push_str(&format!(
            "- `{}` — {}\n  Load it: {}\n",
            skill.name,
            one_line(&skill.description),
            skill.load_instruction()
        ));
    }
    if loaded.is_empty() {
        out.push_str(
            "\nThe engine saw no skill load from this worker's session. That is not a\n\
             failure on its own — a task can be small enough to need none — but if the\n\
             change shows the method was not followed, say which skill and how.\n",
        );
    } else {
        out.push_str(&format!(
            "\nThe engine saw this worker load: {}.\n",
            loaded.join(", ")
        ));
    }
    out
}

/// The lead's shortlist: what exists, and what looks relevant to the goal.
///
/// The lead is given the whole catalogue rather than only the ranked few,
/// because the lead is the one that knows what the tasks will be. The ranking is
/// a starting point, marked as such.
pub fn render_for_lead(catalogue: &Catalogue, ranked: &[Match], limit: usize) -> String {
    if catalogue.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "## Skills\n\
         This machine has skills installed: accumulated method for specific kinds of\n\
         work. Assign the ones each task needs by naming them in the task's `skills`\n\
         array. A worker that is given a skill loads it before it writes, and the\n\
         verifier is told which ones it was given.\n\n",
    );

    if !ranked.is_empty() {
        out.push_str("### Looks relevant to this goal\n");
        for entry in ranked {
            let description = catalogue
                .get(&entry.name)
                .map(|skill| one_line(&skill.description))
                .unwrap_or_default();
            out.push_str(&format!(
                "- `{}` — {} _(matched: {})_\n",
                entry.name,
                description,
                entry.why.join(", ")
            ));
        }
        out.push('\n');
    }

    out.push_str("### Everything installed\n");
    let assignable: Vec<&Skill> = catalogue.assignable().collect();
    for skill in assignable.iter().take(limit) {
        out.push_str(&format!(
            "- `{}` — {}\n",
            skill.name,
            one_line(&skill.description)
        ));
    }
    // The tail is named, not dropped. The rules below say a name that is not in
    // this brief is refused, so a skill the brief omits is a skill the lead
    // cannot assign — and the catalogue is the machine's whole method, not a
    // sample of it. Names only, because the description is what costs tokens and
    // the lead only needs the name to assign it.
    let tail: Vec<&str> = assignable
        .iter()
        .skip(limit)
        .map(|skill| skill.name.as_str())
        .collect();
    if !tail.is_empty() {
        out.push_str(&format!(
            "\n### Also installed, by name only\n\
             These are assignable too; the brief lists them without descriptions to\n\
             keep it readable. Read one before you assign it if the name is not enough.\n\
             {}\n",
            tail.iter()
                .map(|name| format!("`{name}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    out.push_str(
        "\n### The rules\n\
         - Name skills by their exact name, in the task's `skills` array. A name that\n\
           is not in the list above is refused and the plan comes back to you.\n\
         - A task with no `skills` gets the ones the engine ranks for it. That is a\n\
           fallback, not a decision: you know what the task is, and the ranking reads\n\
           its title.\n\
         - Do not assign a skill to a task that does not need it. A brief with five\n\
           skills in it is a brief the worker skims.\n\
         - If the work needs a method this machine does not have, say so in `notes`.\n\
           The run records the gap and the reflection pass proposes a skill for it.\n",
    );
    out
}

/// Collapse a description to one line, because a brief is read as a list.
fn one_line(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= 160 {
        return collapsed;
    }
    let mut out: String = collapsed.chars().take(157).collect();
    out.push_str("...");
    out
}

/* ------------------------------------------------------------------ *
 * Evidence
 * ------------------------------------------------------------------ */

/// The skills a member's own tool calls loaded.
///
/// Mirrors the window's `skill-use.ts`, and for the same reason: the only
/// evidence that a skill was used is the call the agent made to load it. Four
/// shapes exist and all are read —
///
/// - `mcp_mastery.load { bundle, skill }`
/// - `skill { name }`
/// - a read of the skill's own `SKILL.md`, which is how a member loads a skill
///   inside a run: member sessions open with no MCP servers, so the mastery
///   loader is not reachable there and the brief tells the worker to read the
///   path instead. Without this every assignment would read as a gap.
/// - the title `Loading skill: <name>`, which is the app server's own wording for
///   the skill tool and the only evidence left when `rawInput` is not forwarded.
///
/// A call that names none of these is not a load. Guessing from a title would
/// credit a skill the member never opened, which is worse than recording nothing.
pub fn loads_from_tool_call(call: &serde_json::Value) -> Vec<String> {
    const LOADERS: &[&str] = &["mcp_mastery.load", "mcp_mastery_load", "skill", "skills.load"];

    let raw = call.get("rawInput").filter(|v| v.is_object());
    let title = call.get("title").and_then(|v| v.as_str()).unwrap_or("");

    let tool = {
        let mut found = String::new();
        let mut candidates: Vec<String> = Vec::new();
        if let Some(raw) = raw {
            for key in ["tool", "toolName"] {
                if let Some(value) = raw.get(key).and_then(|v| v.as_str()) {
                    candidates.push(value.to_string());
                }
            }
            if let Some(name) = raw.get("name").and_then(|v| v.as_str()) {
                if LOADERS.contains(&name.to_lowercase().as_str()) {
                    candidates.push(name.to_string());
                }
            }
        }
        candidates.push(title.to_string());
        for candidate in candidates {
            let lower = candidate.to_lowercase();
            if LOADERS
                .iter()
                .any(|loader| lower == *loader || lower.starts_with(&format!("{loader} ")))
            {
                found = lower;
                break;
            }
        }
        found
    };

    if !tool.is_empty() {
        if let Some(raw) = raw {
            let bundle = raw.get("bundle").and_then(|v| v.as_str());
            let skill = raw
                .get("skill")
                .and_then(|v| v.as_str())
                .or_else(|| raw.get("skillName").and_then(|v| v.as_str()));
            if let Some(skill) = skill.map(str::trim).filter(|s| !s.is_empty()) {
                let _ = bundle;
                return vec![unqualify(skill)];
            }
            if tool == "skill" || tool == "skills.load" {
                if let Some(name) = raw.get("name").and_then(|v| v.as_str()) {
                    if !name.trim().is_empty() {
                        return vec![unqualify(name.trim())];
                    }
                }
            }
        }
    }

    if let Some(rest) = title
        .strip_prefix("Loading skill:")
        .or_else(|| title.strip_prefix("loading skill:"))
    {
        let name = rest.trim();
        if !name.is_empty() {
            return vec![unqualify(name)];
        }
    }

    // A read of the skill's own file. Every string the call carries is searched,
    // because the path arrives under `rawInput.path` for a read, under
    // `locations[].path` when the agent reports where it went, and inside a shell
    // command when the member opens the file with `cat`.
    let mut candidates: Vec<String> = Vec::new();
    if let Some(raw) = call.get("rawInput") {
        collect_strings(raw, &mut candidates);
    }
    if let Some(locations) = call.get("locations").and_then(|v| v.as_array()) {
        for location in locations {
            if let Some(path) = location.get("path").and_then(|v| v.as_str()) {
                candidates.push(path.to_string());
            }
        }
    }
    candidates.push(title.to_string());
    for candidate in candidates {
        if let Some(name) = skill_from_path(&candidate) {
            return vec![name];
        }
    }

    Vec::new()
}

/// Every string inside a JSON value, in document order.
fn collect_strings(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) => out.push(text.clone()),
        serde_json::Value::Array(items) => {
            for item in items {
                collect_strings(item, out);
            }
        }
        serde_json::Value::Object(fields) => {
            for (_, field) in fields {
                collect_strings(field, out);
            }
        }
        _ => {}
    }
}

/// The skill a path opens, if it opens one.
///
/// Two shapes count: a path to a `SKILL.md`, whose parent directory is the skill,
/// and a markdown file inside a `skills/<name>/` directory, which is a skill's
/// own reference document. Both are the member opening the skill; neither is a
/// guess, because a file named `SKILL.md` is a skill by convention and nothing
/// else in a repository is.
///
/// The name is validated rather than taken on trust: a sentence that happens to
/// contain the words "SKILL.md" must not credit a skill called `the`.
fn skill_from_path(text: &str) -> Option<String> {
    let normalised = text.replace('\\', "/");

    for (index, _) in normalised.match_indices("SKILL.md") {
        let before = normalised[..index].trim_end_matches('/');
        let Some((_, name)) = before.rsplit_once('/') else {
            continue;
        };
        if let Some(name) = skill_name(name) {
            return Some(name);
        }
    }

    if !normalised.ends_with(".md") {
        return None;
    }
    let (_, rest) = normalised.rsplit_once("/skills/")?;
    skill_name(rest.split('/').next().unwrap_or(""))
}

/// A directory name that can be a skill name, or nothing.
fn skill_name(raw: &str) -> Option<String> {
    let name = raw.trim();
    if name.len() < 2 || name.len() > 64 {
        return None;
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return None;
    }
    Some(unqualify(name))
}

/// `vibe-mastery:assay` is the catalog's `assay`.
///
/// The agent names a plugin skill qualified; the catalogue is built from
/// directories and has no prefix. Keying evidence on the qualified name would
/// leave every mastery skill looking unloaded.
fn unqualify(name: &str) -> String {
    match name.split_once(':') {
        Some((_, rest)) if !rest.is_empty() => rest.to_string(),
        _ => name.to_string(),
    }
}

/// Which of the assigned skills a member never loaded.
///
/// The comparison is by name, and a skill counts as loaded if the member loaded
/// it or anything whose name ends with it — the agent sometimes reports
/// `vibe-mastery:assay` and sometimes `assay`, and a mismatch there would report
/// a load that happened as a gap.
pub fn unloaded(assigned: &[String], loaded: &[String]) -> Vec<String> {
    assigned
        .iter()
        .filter(|name| {
            !loaded.iter().any(|seen| {
                seen == *name || seen.ends_with(&format!(":{name}")) || name.ends_with(&format!(":{seen}"))
            })
        })
        .cloned()
        .collect()
}

/* ------------------------------------------------------------------ *
 * Discovery
 * ------------------------------------------------------------------ */

/// A skill the run decided this machine is missing.
///
/// Written by the reflection pass, not by a worker: a worker mid-task that stops
/// to author a skill is a worker not doing its task. The proposal lands in the
/// run's own directory, so nothing is written to the user's skill store without
/// them accepting it — a run that could write to `~/.vibe/skills` could change
/// how every future run behaves, which is not a side effect to take on trust.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Proposal {
    /// The name the skill would have. Lowercase, hyphenated.
    pub name: String,
    pub description: String,
    /// The `SKILL.md` body, without frontmatter.
    pub body: String,
    /// Why the run needed it: the task and the thing that went wrong.
    pub because: String,
    /// Where it was written, so the panel can show the file.
    pub path: String,
}

impl Proposal {
    /// The file this proposal becomes when accepted.
    pub fn render(&self) -> String {
        format!(
            "---\nname: {}\ndescription: {}\n---\n\n{}\n",
            self.name,
            self.description.replace('\n', " "),
            self.body.trim()
        )
    }

    /// Whether the name is one a skill directory can hold.
    ///
    /// A name with a path separator in it would write outside the skills
    /// directory, and a name with a space would produce a directory the CLI's
    /// discovery reads but the agent cannot name.
    pub fn valid_name(&self) -> bool {
        !self.name.is_empty()
            && self.name.len() <= 64
            && self
                .name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(name: &str, description: &str, scope: &str) -> Skill {
        Skill {
            name: name.into(),
            description: description.into(),
            path: format!("C:/skills/{name}/SKILL.md"),
            scope: scope.into(),
            bundle: None,
            enabled: true,
            body: String::new(),
        }
    }

    #[test]
    fn a_prefix_of_a_skill_term_matches_at_less_weight_than_the_whole_word() {
        // `debug` is not `debugging`, and a tokeniser that only compares whole
        // words would hand a debugging task no debugging skill. The prefix rule
        // is what makes the ranking survive the way people actually write goals.
        let cat = Catalogue {
            skills: vec![
                skill(
                    "systematic-debugging",
                    "Find the cause of a bug before proposing a fix.",
                    "user",
                ),
                skill("brainstorming", "Pressure-test a plan before building it.", "user"),
            ],
        };
        let near = rank(&cat, "debug the renderer", 4);
        assert_eq!(near[0].name, "systematic-debugging");
        assert!(near[0].why.iter().any(|why| why == "debug~debugging"));

        // The whole word still outranks the prefix, so a goal that says
        // `debugging` is not beaten by one that says `debug`.
        let exact = rank(&cat, "debugging the renderer", 4);
        assert_eq!(exact[0].name, "systematic-debugging");
        assert!(exact[0].why.iter().any(|why| why == "debugging"));
        assert!(exact[0].score > near[0].score);
    }

    #[test]
    fn a_three_letter_prefix_is_not_a_match() {
        // `the` is a stopword and `add` is too short to be evidence. Without the
        // length floor, `add` would match `address`, `additive` and every other
        // word that starts with it.
        let cat = Catalogue {
            skills: vec![skill("additive-blending", "How additive blending works.", "user")],
        };
        assert!(rank(&cat, "add a button", 4).is_empty());
    }

    fn catalogue() -> Catalogue {
        Catalogue {
            skills: vec![
                skill(
                    "3dviz-pro-max",
                    "Build 3D scenes with three.js: geometry, materials, lighting, animation loops.",
                    "mastery",
                ),
                skill(
                    "assay",
                    "The expensive-path discipline for work where being wrong costs something.",
                    "mastery",
                ),
                skill(
                    "anti-slop",
                    "Reject placeholder copy, lorem ipsum and hollow UI states.",
                    "mastery",
                ),
                skill(
                    "repo-conventions",
                    "This repository's own conventions for modules and tests.",
                    "project",
                ),
            ],
        }
    }

    #[test]
    fn a_goal_about_3d_ranks_the_3d_skill_first() {
        let ranked = rank(&catalogue(), "build a 3D scene with lighting and animation", 4);
        assert_eq!(ranked[0].name, "3dviz-pro-max");
        assert!(ranked[0].why.iter().any(|why| why == "3d"));
    }

    #[test]
    fn a_named_skill_outranks_any_inference() {
        // "assay" appears nowhere in the description of the 3D skill, and the
        // goal is about 3D — but the user named assay, so assay wins.
        let ranked = rank(&catalogue(), "build a 3D scene, use assay", 4);
        assert_eq!(ranked[0].name, "assay");
        assert!(ranked[0].why[0].starts_with("named:"));
    }

    #[test]
    fn a_goal_with_nothing_in_common_ranks_nothing() {
        let ranked = rank(&catalogue(), "rename the variable", 4);
        assert!(ranked.is_empty(), "got {ranked:?}");
    }

    #[test]
    fn a_function_word_is_not_a_term() {
        // `to` is two characters, so it cleared the length floor, and it is in
        // almost every description. It was the highest-scoring term for a goal
        // about a minimap and put six arbitrary skills at the top of the
        // shortlist. A word that carries no meaning cannot be evidence.
        assert!(terms("add a minimap to the game").iter().all(|t| t != "to"));
        assert!(terms("the frame of it").iter().all(|t| t != "of"));
        assert!(terms("a scene in three.js").iter().all(|t| t != "in"));
    }

    #[test]
    fn an_ordinary_word_that_appears_in_most_bodies_does_not_rank_a_skill() {
        // The defect this replaced: `stop` appears in one of thirty-one
        // descriptions, so inverse document frequency treated it as a rare,
        // meaningful term and ranked the anti-slop skill first for a goal that
        // said "make the HUD stop flickering". In the bodies it appears in
        // twenty-four of thirty-one — it is an ordinary English word.
        let mut cat = catalogue();
        for skill in &mut cat.skills {
            skill.body = "Stop and check before you continue. Stop when it is done.".into();
        }
        cat.skills[1].description = "Reject placeholder copy and hollow UI states.".into();

        let ranked = rank(&cat, "make the HUD stop flickering", 4);
        // Every skill scores the same, because the only shared term is one every
        // skill's body contains. That is the point: the word carries no
        // information, so it cannot separate one skill from another.
        assert!(
            ranked.iter().all(|entry| entry.score < ASSIGN_THRESHOLD),
            "an ordinary word ranked a skill: {ranked:?}"
        );
        assert!(assign(&cat, "make the HUD stop flickering", 3).is_empty());
    }

    #[test]
    fn a_body_term_ranks_a_skill_its_description_never_mentions() {
        // `3dviz-pro-max` describes itself as "3D scenes and interactive
        // models". The word `three` is only in its body, and a goal about a
        // browser game uses the body's vocabulary, not the description's.
        let mut cat = catalogue();
        cat.skills[0].description = "Design expressive scenes and interactive models.".into();
        cat.skills[0].body = "Build with three.js: geometry, materials, lighting, camera.".into();

        let ranked = rank(&cat, "a three.js scene with a camera", 4);
        assert_eq!(ranked[0].name, "3dviz-pro-max", "got {ranked:?}");
        assert!(ranked[0].why.iter().any(|why| why == "three"));
    }

    #[test]
    fn a_description_hit_outranks_a_body_hit_of_the_same_rarity() {
        // Both skills mention `lighting` exactly once in the library, so the
        // rarity is equal and the tier is the only difference. The skill that
        // says it is for lighting in its own description must win.
        let cat = Catalogue {
            skills: vec![
                Skill {
                    name: "scene-builder".into(),
                    description: "Build a scene with lighting.".into(),
                    body: String::new(),
                    path: "C:/skills/scene-builder/SKILL.md".into(),
                    scope: "mastery".into(),
                    bundle: None,
                    enabled: true,
                },
                Skill {
                    name: "review-notes".into(),
                    description: "How to write a review.".into(),
                    body: "Mention lighting when the change touches it.".into(),
                    path: "C:/skills/review-notes/SKILL.md".into(),
                    scope: "user".into(),
                    bundle: None,
                    enabled: true,
                },
            ],
        };
        let ranked = rank(&cat, "lighting", 4);
        assert_eq!(ranked[0].name, "scene-builder", "got {ranked:?}");
        assert!(ranked[0].score > ranked[1].score);
    }

    #[test]
    fn a_body_is_not_carried_into_the_run_document() {
        // The catalogue is serialized into every run. A body is a thing to read
        // from disk when it is needed, not a thing to copy into every run.
        let mut cat = catalogue();
        cat.skills[0].body = "a long procedure".into();
        let json = serde_json::to_string(&cat).unwrap();
        assert!(!json.contains("a long procedure"));
        let back: Catalogue = serde_json::from_str(&json).unwrap();
        assert!(back.skills[0].body.is_empty());
        assert_eq!(back.skills[0].name, "3dviz-pro-max");
    }

    #[test]
    fn a_disabled_skill_is_never_ranked() {
        let mut catalogue = catalogue();
        catalogue.skills[0].enabled = false;
        let ranked = rank(&catalogue, "build a 3D scene with lighting", 4);
        assert!(ranked.iter().all(|entry| entry.name != "3dviz-pro-max"));
    }

    #[test]
    fn assignment_drops_matches_below_the_threshold() {
        let assigned = assign(&catalogue(), "build a 3D scene with lighting and animation", 3);
        assert_eq!(assigned, vec!["3dviz-pro-max".to_string()]);
    }

    #[test]
    fn a_task_is_ranked_by_its_files_as_well_as_its_title() {
        // Neither the title nor the detail says anything the 3D skill knows. The
        // file does: `lighting` is in its description. Without the file in the
        // ranked text this task would be assigned nothing.
        let bare = task_text("wire it up", "connect the pieces", &[]);
        assert!(assign(&catalogue(), &bare, 3).is_empty());

        let with_file = task_text("wire it up", "connect the pieces", &["src/lighting.ts".into()]);
        let assigned = assign(&catalogue(), &with_file, 3);
        assert!(assigned.contains(&"3dviz-pro-max".to_string()), "got {assigned:?}");
    }

    #[test]
    fn a_mastery_skill_is_loaded_by_path_because_the_member_has_no_mastery_tool() {
        let mastery = skill("assay", "a procedure", "mastery");
        assert!(!mastery.tool_loadable());
        assert!(mastery.load_instruction().starts_with("read `"));
        assert!(!mastery.load_instruction().contains("skill { name"));

        let project = skill("repo-conventions", "conventions", "project");
        assert!(project.tool_loadable());
        assert!(project.load_instruction().contains("skill { name"));
    }

    #[test]
    fn the_worker_section_names_every_skill_and_its_mechanism() {
        let catalogue = catalogue();
        let picked: Vec<&Skill> = vec![catalogue.get("3dviz-pro-max").unwrap()];
        let rendered = render_for_worker(&picked);
        assert!(rendered.contains("3dviz-pro-max"));
        assert!(rendered.contains("C:/skills/3dviz-pro-max/SKILL.md"));
        assert!(rendered.contains("must load"));
    }

    #[test]
    fn an_empty_assignment_renders_nothing_rather_than_an_empty_heading() {
        assert!(render_for_worker(&[]).is_empty());
        assert!(render_for_verifier(&[], &[]).is_empty());
    }

    #[test]
    fn the_verifier_is_told_what_the_worker_actually_loaded() {
        let catalogue = catalogue();
        let picked: Vec<&Skill> = vec![catalogue.get("assay").unwrap()];
        let loaded = vec!["assay".to_string()];
        let rendered = render_for_verifier(&picked, &loaded);
        assert!(rendered.contains("saw this worker load: assay"));

        let none = render_for_verifier(&picked, &[]);
        assert!(none.contains("saw no skill load"));
    }

    #[test]
    fn the_lead_sees_the_shortlist_and_the_whole_catalogue() {
        let catalogue = catalogue();
        let ranked = rank(&catalogue, "build a 3D scene", 2);
        let rendered = render_for_lead(&catalogue, &ranked, 40);
        assert!(rendered.contains("Looks relevant to this goal"));
        assert!(rendered.contains("Everything installed"));
        assert!(rendered.contains("repo-conventions"));
        assert!(rendered.contains("matched:"));
    }

    #[test]
    fn the_lead_catalogue_names_every_skill_even_past_the_description_limit() {
        // The rules say a name that is not in the brief is refused, so a skill
        // the brief omits is a skill the lead cannot assign. The limit is on
        // descriptions, not on names.
        let mut catalogue = catalogue();
        for index in 0..30 {
            catalogue
                .skills
                .push(skill(&format!("extra-{index}"), "filler", "user"));
        }
        let rendered = render_for_lead(&catalogue, &[], 5);
        assert!(rendered.contains("Also installed, by name only"), "got {rendered}");
        for index in 0..30 {
            assert!(
                rendered.contains(&format!("`extra-{index}`")),
                "extra-{index} is unnameable, so it cannot be assigned"
            );
        }
        // The tail carries names, not descriptions: that is the whole point of
        // splitting it out.
        assert!(!rendered.contains("`extra-29` — filler"));
    }

    #[test]
    fn a_skill_load_is_read_from_the_skill_tool() {
        let call = serde_json::json!({
            "title": "skill",
            "rawInput": { "name": "assay" }
        });
        assert_eq!(loads_from_tool_call(&call), vec!["assay".to_string()]);
    }

    #[test]
    fn a_mastery_load_is_read_and_unqualified() {
        let call = serde_json::json!({
            "title": "mcp_mastery.load",
            "rawInput": { "bundle": "assay", "skill": "vibe-mastery:assay" }
        });
        assert_eq!(loads_from_tool_call(&call), vec!["assay".to_string()]);
    }

    #[test]
    fn a_title_alone_is_enough_when_raw_input_is_missing() {
        let call = serde_json::json!({ "title": "Loading skill: 3dviz-pro-max" });
        assert_eq!(loads_from_tool_call(&call), vec!["3dviz-pro-max".to_string()]);
    }

    #[test]
    fn a_read_call_with_a_name_is_not_a_skill_load() {
        let call = serde_json::json!({
            "title": "read",
            "rawInput": { "name": "src/main.rs" }
        });
        assert!(loads_from_tool_call(&call).is_empty());
    }

    #[test]
    fn reading_a_skill_file_is_a_load() {
        // The shape a member actually produces: no MCP servers in the session, so
        // the brief says "read this path" and the agent reads it.
        let call = serde_json::json!({
            "title": "read",
            "kind": "read",
            "rawInput": { "path": "C:\\Users\\Santera\\.vibe\\skills\\systematic-debugging\\SKILL.md" }
        });
        assert_eq!(
            loads_from_tool_call(&call),
            vec!["systematic-debugging".to_string()]
        );
    }

    #[test]
    fn a_mastery_skill_file_is_a_load_too() {
        let call = serde_json::json!({
            "title": "read",
            "rawInput": { "path": "C:/Users/Santera/.vibe/mastery/sources/Anti-Slop/skills/antislop/SKILL.md" }
        });
        assert_eq!(loads_from_tool_call(&call), vec!["antislop".to_string()]);
    }

    #[test]
    fn a_location_is_enough_when_raw_input_is_not_forwarded() {
        let call = serde_json::json!({
            "title": "read",
            "locations": [{ "path": "C:/Users/Santera/.vibe/skills/assay/SKILL.md" }]
        });
        assert_eq!(loads_from_tool_call(&call), vec!["assay".to_string()]);
    }

    #[test]
    fn a_shell_command_that_opens_a_skill_file_is_a_load() {
        let call = serde_json::json!({
            "title": "bash",
            "rawInput": { "command": "cat \"C:/Users/Santera/.vibe/skills/assay/SKILL.md\"" }
        });
        assert_eq!(loads_from_tool_call(&call), vec!["assay".to_string()]);
    }

    #[test]
    fn a_reference_file_inside_a_skill_directory_is_a_load() {
        let call = serde_json::json!({
            "title": "read",
            "rawInput": { "path": "C:/Users/Santera/.vibe/skills/3dviz-pro-max/reference/materials.md" }
        });
        assert_eq!(loads_from_tool_call(&call), vec!["3dviz-pro-max".to_string()]);
    }

    #[test]
    fn prose_that_mentions_a_skill_file_is_not_a_load() {
        let call = serde_json::json!({
            "title": "write",
            "rawInput": { "content": "Read the SKILL.md file before you start." }
        });
        assert!(loads_from_tool_call(&call).is_empty());
    }

    #[test]
    fn an_ordinary_markdown_read_is_not_a_load() {
        let call = serde_json::json!({
            "title": "read",
            "rawInput": { "path": "D:/VibeDesk/VibeDeskProjects/orbital/README.md" }
        });
        assert!(loads_from_tool_call(&call).is_empty());
    }

    #[test]
    fn a_gap_is_the_assigned_skill_that_was_never_loaded() {
        let assigned = vec!["assay".to_string(), "3dviz-pro-max".to_string()];
        let loaded = vec!["assay".to_string()];
        assert_eq!(unloaded(&assigned, &loaded), vec!["3dviz-pro-max".to_string()]);
    }

    #[test]
    fn a_qualified_load_closes_the_gap_for_the_bare_name() {
        let assigned = vec!["assay".to_string()];
        let loaded = vec!["vibe-mastery:assay".to_string()];
        assert!(unloaded(&assigned, &loaded).is_empty());
    }

    #[test]
    fn a_proposal_renders_frontmatter_the_cli_can_read() {
        let proposal = Proposal {
            name: "scene-graph".into(),
            description: "How this repo builds a scene graph".into(),
            body: "Do the thing.".into(),
            because: "t2 invented it".into(),
            path: String::new(),
        };
        let rendered = proposal.render();
        assert!(rendered.starts_with("---\nname: scene-graph\n"));
        assert!(rendered.contains("description: How this repo builds a scene graph"));
        assert!(rendered.contains("Do the thing."));
    }

    #[test]
    fn a_proposal_name_that_could_escape_the_skills_directory_is_refused() {
        let mut proposal = Proposal {
            name: "../evil".into(),
            description: String::new(),
            body: String::new(),
            because: String::new(),
            path: String::new(),
        };
        assert!(!proposal.valid_name());
        proposal.name = "has space".into();
        assert!(!proposal.valid_name());
        proposal.name = "scene-graph".into();
        assert!(proposal.valid_name());
    }

    #[test]
    fn a_description_is_collapsed_to_one_line_in_a_brief() {
        let long = "a ".repeat(200);
        let collapsed = one_line(&long);
        assert!(collapsed.ends_with("..."));
        assert!(collapsed.chars().count() <= 160);
    }
}
