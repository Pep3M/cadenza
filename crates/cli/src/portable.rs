//! `cadenza collect`, `relink`, `pack` and `unpack`: portable project folders.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use cadenza_mcp::Session;
use cadenza_mcp::tools::portable;

/// Opens `project` into a fresh session (plugin catalog untouched).
fn open(project: &Path) -> Result<Session> {
	let (document, _) = portable::open_document(project)
		.with_context(|| format!("loading project {}", project.display()))?;
	let mut session = Session::new();
	session.reset(document, Some(project.to_path_buf()));
	Ok(session)
}

fn save(session: &Session, project: &Path) -> Result<()> {
	session
		.document()
		.project()
		.save_json(project)
		.with_context(|| format!("saving {}", project.display()))
}

fn mib(bytes: u64) -> String {
	format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

pub fn collect(project: &Path, include_factory: bool, dry_run: bool) -> Result<()> {
	let mut session = open(project)?;
	let out = portable::collect_files(
		&mut session,
		portable::CollectFilesArgs {
			include_factory: Some(include_factory),
			dry_run: Some(dry_run),
		},
	)?;
	if !dry_run && out.references_rewritten > 0 {
		save(&session, project)?;
	}
	let planned: u64 = out.copied.iter().map(|c| c.bytes).sum();
	eprintln!(
		"{} {} file(s) ({}), {} deduplicated, {} already inside, {} factory sample(s) left shared{}",
		if dry_run { "would copy" } else { "copied" },
		out.copied.len(),
		mib(planned),
		out.deduplicated,
		out.already_inside,
		out.factory_skipped,
		if out.missing.is_empty() {
			String::new()
		} else {
			format!(", {} missing", out.missing.len())
		}
	);
	crate::print_json(&out)
}

pub fn relink(project: &Path, search: &Path, dry_run: bool) -> Result<()> {
	let mut session = open(project)?;
	let out = portable::relink_files(
		&mut session,
		portable::RelinkFilesArgs {
			search_folder: search.display().to_string(),
			dry_run: Some(dry_run),
		},
	)?;
	if !dry_run && !out.relinked.is_empty() {
		save(&session, project)?;
	}
	eprintln!(
		"{} relinked, {} still missing ({} files searched)",
		out.relinked.len(),
		out.still_missing.len(),
		out.files_searched
	);
	crate::print_json(&out)
}

pub fn pack(project: &Path, output: Option<PathBuf>, include_renders: bool) -> Result<()> {
	let session = open(project)?;
	let output = output.unwrap_or_else(|| {
		let stem = cadenza_model::files::project_stem(project);
		project.with_file_name(format!("{stem}.cdzpack"))
	});
	let out = portable::pack_current(
		&session,
		portable::PackProjectArgs {
			path: output.display().to_string(),
			include_renders: Some(include_renders),
		},
	)?;
	eprintln!(
		"wrote {} ({} files, {})",
		out.path,
		out.files,
		mib(out.bytes)
	);
	if !out.outside.is_empty() {
		eprintln!(
			"warning: {} file(s) outside the project folder are not in the archive; run \
			 `cadenza collect` first",
			out.outside.len()
		);
	}
	crate::print_json(&out)
}

pub fn unpack(archive: &Path, dest: &Path) -> Result<()> {
	if !portable::is_archive(archive) {
		bail!(
			"'{}' is not a project archive (.cdzpack)",
			archive.display()
		);
	}
	let project = portable::unpack_project(archive, dest)?;
	println!("{}", project.display());
	Ok(())
}
