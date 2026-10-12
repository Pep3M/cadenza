//! API keys: resolved from the environment, then the nearest `.env`, then the key the
//! user stored in the app. Stored keys go to the OS secret store (Secret Service on Linux,
//! Keychain, Credential Manager) through `keyring`; when no secret store is running they
//! fall back to `$XDG_CONFIG_HOME/cadenza/credentials.json` with 0600 permissions. Keys
//! are never written to project files, logs, transcripts or shown in clear.
//!
//! Every function here may block (D-Bus): call them from the AI worker thread, never
//! from the UI thread.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use cadenza_agent::config::{find_dotenv, parse_dotenv};

use super::settings::ProviderKind;

const SERVICE: &str = "cadenza";

/// Where a key was found (shown in the settings, the key itself never is).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySource {
	Env(&'static str),
	DotEnv(PathBuf),
	/// Stored by the app; the text names the store.
	Stored(String),
	Missing,
}

impl KeySource {
	pub fn describe(&self) -> String {
		match self {
			KeySource::Env(name) => format!("from the environment ({name})"),
			KeySource::DotEnv(path) => format!("from {}", path.display()),
			KeySource::Stored(store) => format!("stored in {store}"),
			KeySource::Missing => "not set".into(),
		}
	}
}

pub trait SecretStore: Send + Sync {
	fn get(&self, account: &str) -> Result<Option<String>, String>;
	fn set(&self, account: &str, secret: &str) -> Result<(), String>;
	fn delete(&self, account: &str) -> Result<(), String>;
	/// Human name of where keys go ("the system keyring").
	fn name(&self) -> String;
}

/// Looks up the key for `provider`: environment variable, nearest `.env` above
/// `start_dir`, then `store`.
pub fn resolve_key(
	provider: ProviderKind,
	start_dir: &Path,
	store: &dyn SecretStore,
) -> (Option<String>, KeySource) {
	let env = provider.key_env();
	if let Ok(value) = std::env::var(env)
		&& !value.trim().is_empty()
	{
		return (Some(value.trim().to_owned()), KeySource::Env(env));
	}
	if let Some(file) = find_dotenv(start_dir)
		&& let Ok(content) = std::fs::read_to_string(&file)
		&& let Some((_, value)) = parse_dotenv(&content)
			.into_iter()
			.find(|(k, v)| k == env && !v.is_empty())
	{
		return (Some(value), KeySource::DotEnv(file));
	}
	match store.get(provider.account()) {
		Ok(Some(key)) if !key.trim().is_empty() => (Some(key), KeySource::Stored(store.name())),
		_ => (None, KeySource::Missing),
	}
}

/// The OS secret store, with the 0600 file as fallback.
pub struct SystemSecretStore {
	file: FileSecretStore,
}

impl SystemSecretStore {
	pub fn new() -> Self {
		Self {
			file: FileSecretStore::new(default_credentials_path()),
		}
	}

	fn keyring_entry(account: &str) -> Option<keyring::Entry> {
		match keyring::Entry::store_status() {
			Ok(()) => keyring::Entry::new(SERVICE, account).ok(),
			Err(_) => None,
		}
	}
}

impl Default for SystemSecretStore {
	fn default() -> Self {
		Self::new()
	}
}

impl SecretStore for SystemSecretStore {
	fn get(&self, account: &str) -> Result<Option<String>, String> {
		if let Some(entry) = Self::keyring_entry(account) {
			match entry.get_password() {
				Ok(key) => return Ok(Some(key)),
				Err(keyring::Error::NoEntry) => {}
				Err(_) => {}
			}
		}
		self.file.get(account)
	}

	fn set(&self, account: &str, secret: &str) -> Result<(), String> {
		if let Some(entry) = Self::keyring_entry(account)
			&& entry.set_password(secret).is_ok()
		{
			// A key stored earlier in the fallback file must not shadow the new one.
			let _ = self.file.delete(account);
			return Ok(());
		}
		self.file.set(account, secret)
	}

	fn delete(&self, account: &str) -> Result<(), String> {
		if let Some(entry) = Self::keyring_entry(account) {
			let _ = entry.delete_credential();
		}
		self.file.delete(account)
	}

	fn name(&self) -> String {
		if keyring::Entry::store_status().is_ok() {
			"the system keyring".into()
		} else {
			format!("{} (mode 0600)", self.file.path.display())
		}
	}
}

/// `$XDG_CONFIG_HOME/cadenza/credentials.json` (or `~/.config/...`).
pub fn default_credentials_path() -> PathBuf {
	std::env::var_os("XDG_CONFIG_HOME")
		.map(PathBuf::from)
		.filter(|p| p.is_absolute())
		.or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
		.unwrap_or_else(std::env::temp_dir)
		.join("cadenza")
		.join("credentials.json")
}

/// Keys in a JSON file readable only by the user.
pub struct FileSecretStore {
	path: PathBuf,
	lock: Mutex<()>,
}

impl FileSecretStore {
	pub fn new(path: PathBuf) -> Self {
		Self {
			path,
			lock: Mutex::new(()),
		}
	}

	fn read(&self) -> BTreeMap<String, String> {
		std::fs::read_to_string(&self.path)
			.ok()
			.and_then(|t| serde_json::from_str(&t).ok())
			.unwrap_or_default()
	}

	fn write(&self, map: &BTreeMap<String, String>) -> Result<(), String> {
		if let Some(dir) = self.path.parent() {
			std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
		}
		let text = serde_json::to_string_pretty(map).map_err(|e| e.to_string())?;
		let tmp = self.path.with_extension("json.tmp");
		{
			use std::io::Write;
			let mut options = std::fs::OpenOptions::new();
			options.write(true).create(true).truncate(true);
			#[cfg(unix)]
			{
				use std::os::unix::fs::OpenOptionsExt;
				options.mode(0o600);
			}
			let mut file = options.open(&tmp).map_err(|e| e.to_string())?;
			#[cfg(unix)]
			{
				use std::os::unix::fs::PermissionsExt;
				file.set_permissions(std::fs::Permissions::from_mode(0o600))
					.map_err(|e| e.to_string())?;
			}
			file.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
		}
		std::fs::rename(&tmp, &self.path).map_err(|e| e.to_string())
	}
}

impl SecretStore for FileSecretStore {
	fn get(&self, account: &str) -> Result<Option<String>, String> {
		let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
		Ok(self.read().get(account).cloned())
	}

	fn set(&self, account: &str, secret: &str) -> Result<(), String> {
		let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
		let mut map = self.read();
		map.insert(account.to_owned(), secret.to_owned());
		self.write(&map)
	}

	fn delete(&self, account: &str) -> Result<(), String> {
		let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
		let mut map = self.read();
		if map.remove(account).is_some() {
			self.write(&map)?;
		}
		Ok(())
	}

	fn name(&self) -> String {
		format!("{} (mode 0600)", self.path.display())
	}
}

/// Keys in memory only (tests, sessions that must not persist anything).
#[derive(Default)]
pub struct MemorySecretStore(Mutex<BTreeMap<String, String>>);

impl SecretStore for MemorySecretStore {
	fn get(&self, account: &str) -> Result<Option<String>, String> {
		Ok(self
			.0
			.lock()
			.unwrap_or_else(|p| p.into_inner())
			.get(account)
			.cloned())
	}

	fn set(&self, account: &str, secret: &str) -> Result<(), String> {
		self.0
			.lock()
			.unwrap_or_else(|p| p.into_inner())
			.insert(account.to_owned(), secret.to_owned());
		Ok(())
	}

	fn delete(&self, account: &str) -> Result<(), String> {
		self.0
			.lock()
			.unwrap_or_else(|p| p.into_inner())
			.remove(account);
		Ok(())
	}

	fn name(&self) -> String {
		"memory".into()
	}
}

/// `sk-or-v1-abcd…` → `••••••••` (nothing of the key is shown).
pub fn mask(key: &str) -> String {
	if key.is_empty() {
		String::new()
	} else {
		"•".repeat(12)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn the_file_store_is_private_and_round_trips() {
		let dir = tempfile::tempdir().unwrap();
		let store = FileSecretStore::new(dir.path().join("cfg/credentials.json"));
		assert_eq!(store.get("a").unwrap(), None);
		store.set("a", "secret-1").unwrap();
		store.set("b", "secret-2").unwrap();
		assert_eq!(store.get("a").unwrap().as_deref(), Some("secret-1"));
		#[cfg(unix)]
		{
			use std::os::unix::fs::PermissionsExt;
			let mode = std::fs::metadata(dir.path().join("cfg/credentials.json"))
				.unwrap()
				.permissions()
				.mode();
			assert_eq!(mode & 0o777, 0o600);
		}
		store.delete("a").unwrap();
		assert_eq!(store.get("a").unwrap(), None);
		assert_eq!(store.get("b").unwrap().as_deref(), Some("secret-2"));
		assert!(!store.name().contains("secret"));
	}

	#[test]
	fn resolution_prefers_env_then_dotenv_then_the_store() {
		// The OpenAI-compatible variable is not set in the test environment.
		if std::env::var_os(ProviderKind::OpenAiCompatible.key_env()).is_some() {
			return;
		}
		let dir = tempfile::tempdir().unwrap();
		let store = MemorySecretStore::default();
		let p = ProviderKind::OpenAiCompatible;
		assert_eq!(
			resolve_key(p, dir.path(), &store),
			(None, KeySource::Missing)
		);
		store.set(p.account(), "stored").unwrap();
		let (key, source) = resolve_key(p, dir.path(), &store);
		assert_eq!(key.as_deref(), Some("stored"));
		assert_eq!(source, KeySource::Stored("memory".into()));
		std::fs::write(dir.path().join(".env"), "OPENAI_API_KEY=from-file\n").unwrap();
		let (key, source) = resolve_key(p, dir.path(), &store);
		assert_eq!(key.as_deref(), Some("from-file"));
		assert!(matches!(source, KeySource::DotEnv(_)));
		assert!(!source.describe().contains("from-file"));
		assert_eq!(mask("sk-123"), "••••••••••••");
	}
}
