//! Folder skills in the Agent Skills shape (`<name>/SKILL.md` with companion files) beside
//! Boubou's flat `<name>.md`, the conventional directories, and the frontmatter the
//! convention writes (bead boubou-w3i.1).

use std::path::Path;

use boubou_kernel::exec::Executor;
use boubou_kernel::safety::SafetyPolicy;
use boubou_kernel::skills::{split_frontmatter, standard_dirs, Skill, SkillSet};
use serde_json::json;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, text).expect("write");
}

/// A folder skill as the convention lays it out: SKILL.md, a script, a reference, and a
/// hidden file that is nobody's business.
fn folder_skill(dir: &Path, name: &str, frontmatter: &str) {
    write(
        &dir.join(name).join("SKILL.md"),
        &format!("{frontmatter}\n# {name}\nRun the script, then read the reference.\n"),
    );
    write(
        &dir.join(name).join("scripts").join("run.sh"),
        "#!/bin/sh\necho hi\n",
    );
    write(
        &dir.join(name).join("references").join("guide.md"),
        "# Guide\n",
    );
    write(&dir.join(name).join(".DS_Store"), "");
}

#[test]
fn a_folder_skill_is_discovered_beside_flat_ones_and_lists_its_files_on_load() {
    let d = tempfile::tempdir().expect("tmp");
    let skills = d.path().join("skills");
    write(
        &skills.join("flat.md"),
        "---\nname: flat\ndescription: a flat one\n---\nFlat body.\n",
    );
    folder_skill(
        &skills,
        "deploy",
        "---\nname: deploy\ndescription: Ship a release\n---",
    );
    write(&skills.join("README.md"), "# not a skill");
    write(
        &skills.join("notes").join("todo.txt"),
        "a folder without SKILL.md",
    );

    let set = SkillSet::discover([&skills]);
    assert_eq!(set.listing(), "deploy: Ship a release\nflat: a flat one");

    let flat = set.get("flat").expect("flat");
    assert_eq!(flat.dir(), None);
    assert!(flat.companion_files().is_empty());
    assert_eq!(flat.load().expect("load"), "Flat body.\n");

    let deploy = set.get("deploy").expect("deploy");
    assert_eq!(deploy.dir(), Some(skills.join("deploy").as_path()));
    let files = deploy.companion_files();
    assert_eq!(
        files,
        vec![
            skills.join("deploy").join("references").join("guide.md"),
            skills.join("deploy").join("scripts").join("run.sh"),
        ],
        "sorted, SKILL.md and hidden files left out"
    );
    let body = deploy.load().expect("load");
    assert!(body.starts_with("# deploy\nRun the script"), "{body}");
    assert!(
        body.contains(
            "Files in this skill's folder (use the full path with read_file or run_bash):\n- "
        ),
        "{body}"
    );
    assert!(
        body.contains(&format!(
            "\n- {}\n",
            skills
                .join("deploy")
                .join("scripts")
                .join("run.sh")
                .display()
        )),
        "{body}"
    );
}

#[test]
fn a_folder_skill_without_a_name_takes_its_folder_name() {
    let d = tempfile::tempdir().expect("tmp");
    folder_skill(
        d.path(),
        "beads",
        "---\ndescription: Track work with bd\n---",
    );
    let s = Skill::from_dir(&d.path().join("beads")).expect("parse");
    assert_eq!(s.name, "beads");
    assert_eq!(s.description, "Track work with bd");
    // A frontmatter with neither is still a format error.
    write(
        &d.path().join("bare").join("SKILL.md"),
        "---\nlicense: MIT\n---\nbody",
    );
    assert!(Skill::from_dir(&d.path().join("bare")).is_err());
}

#[test]
fn the_conventions_frontmatter_parses() {
    // A folded description, a nested metadata block and keys Boubou does not act on.
    let text = "---\nname: beads\ndescription: >\n  Use when working in a repository that uses bd.\n  Trigger when the user asks to find ready work.\nlicense: Apache-2.0\nallowed-tools: Bash(bd:*) Read\nmetadata:\n  author: steveyegge\n  version: \"1.2\"\n---\n# Beads\n";
    let (meta, body) = split_frontmatter(text).expect("frontmatter");
    assert_eq!(
        meta["description"],
        "Use when working in a repository that uses bd. Trigger when the user asks to find ready work."
    );
    assert_eq!(meta["metadata"], "");
    assert_eq!(meta["metadata.author"], "steveyegge");
    assert_eq!(meta["metadata.version"], "\"1.2\"");
    assert_eq!(meta["allowed-tools"], "Bash(bd:*) Read");
    assert_eq!(body, "# Beads\n");

    let d = tempfile::tempdir().expect("tmp");
    write(&d.path().join("beads").join("SKILL.md"), text);
    let s = Skill::from_dir(&d.path().join("beads")).expect("parse");
    assert_eq!(s.name, "beads");
    assert!(s.description.starts_with("Use when working"));
    assert!(
        s.triggers.is_empty() && !s.autoload,
        "no Boubou keys, nothing prefilled"
    );

    // Boubou's own flat frontmatter is unchanged, triggers included.
    let (meta, _) = split_frontmatter(
        "---\nname: finance\ndescription: Ledgers\ntriggers: ledger, invoice\nautoload: false\n---\nx",
    )
    .expect("frontmatter");
    assert_eq!(meta["triggers"], "ledger, invoice");
    assert_eq!(meta["description"], "Ledgers");
}

#[test]
fn the_conventional_directories_follow_the_projects_own_and_precede_the_home() {
    let work = Path::new("/w");
    let home = Path::new("/h");
    let install = Path::new("/i");
    let dirs = standard_dirs(work, home, install);
    let shown: Vec<String> = dirs.iter().map(|p| p.display().to_string()).collect();
    assert_eq!(shown[0], "/w/skills");
    assert_eq!(shown[1], "/w/.agents/skills");
    assert_eq!(shown[2], "/w/.claude/skills");
    assert_eq!(shown[3], "/h/skills");
    assert_eq!(shown.last().expect("install"), "/i/skills");
    if let Some(user_home) = dirs::home_dir() {
        assert!(shown.contains(&user_home.join(".agents/skills").display().to_string()));
        assert!(shown.contains(&user_home.join(".claude/skills").display().to_string()));
    }

    // Precedence in practice: the project's `skills/` shadows `.agents/skills`, which
    // shadows `.claude/skills`.
    let d = tempfile::tempdir().expect("tmp");
    let w = d.path();
    write(
        &w.join("skills").join("a.md"),
        "---\nname: a\ndescription: project flat\n---\nx",
    );
    folder_skill(
        &w.join(".agents").join("skills"),
        "a",
        "---\nname: a\ndescription: agents folder\n---",
    );
    folder_skill(
        &w.join(".agents").join("skills"),
        "b",
        "---\nname: b\ndescription: agents b\n---",
    );
    folder_skill(
        &w.join(".claude").join("skills"),
        "b",
        "---\nname: b\ndescription: claude b\n---",
    );
    folder_skill(
        &w.join(".claude").join("skills"),
        "c",
        "---\nname: c\ndescription: claude c\n---",
    );
    let set = SkillSet::discover(standard_dirs(w, &w.join("nohome"), &w.join("noinstall")));
    assert_eq!(set.get("a").expect("a").description, "project flat");
    assert_eq!(set.get("b").expect("b").description, "agents b");
    assert_eq!(set.get("c").expect("c").description, "claude c");
}

#[test]
fn the_skills_tool_lists_and_loads_a_folder_skill_from_the_project() {
    let d = tempfile::tempdir().expect("tmp");
    folder_skill(
        &d.path().join(".agents").join("skills"),
        "release_notes",
        "---\nname: release_notes\ndescription: Write the notes for a release\n---",
    );
    let mut e = Executor::with_policy(d.path(), SafetyPolicy::default());
    let listed = e.run_tool("skills", &json!({"action": "list"}));
    assert!(
        listed.contains("• release_notes: Write the notes for a release"),
        "{listed}"
    );
    let loaded = e.run_tool(
        "skills",
        &json!({"action": "load", "name": "release_notes"}),
    );
    assert!(
        loaded.starts_with("### SKILL: release_notes\n# release_notes\n"),
        "{loaded}"
    );
    let script = d.path().join(".agents/skills/release_notes/scripts/run.sh");
    assert!(
        loaded.contains(&format!("- {}", script.display())),
        "{loaded}"
    );
    // The listed path works as the model would use it.
    let read = e.run_tool("read_file", &json!({"path": script.display().to_string()}));
    assert_eq!(read, "#!/bin/sh\necho hi\n");
}
