//! Exo's OAuth clients: Google's downloaded client JSON for a "Desktop app" client, and
//! Microsoft's ([`MicrosoftClient`]).
//!
//! Exo ships no Google client of its own. Everyone creates theirs in Google Cloud Console and
//! puts the JSON where [`ClientConfig::load`] looks. Exo signs in to Microsoft as itself unless
//! a file in the same folder names another registration.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use gmail::Endpoints;
use gmail::oauth::OAuthClient;
use serde::Deserialize;

use crate::{Error, MicrosoftClient};

/// The variable naming a client file, which wins over the config folder's.
pub(crate) const ENV: &str = "EXO_GOOGLE_OAUTH_CLIENT";
/// The client file's name in Exo's config folder.
pub(crate) const FILE_NAME: &str = "google-oauth-client.json";

/// Why Google's client JSON wasn't loaded, kept so attempting Google sign-in reports where to put
/// the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MissingGoogle {
    pub(crate) path: PathBuf,
    pub(crate) from_env: bool,
}

impl MissingGoogle {
    pub(crate) fn to_error(&self) -> Error {
        Error::MissingClient {
            path: self.path.clone(),
            from_env: self.from_env,
        }
    }
}

#[derive(Clone)]
struct GoogleClient {
    client_id: String,
    /// Google issues Desktop clients a secret and expects it, though it isn't confidential. It's
    /// still never printed.
    client_secret: String,
}

#[derive(Clone)]
enum GoogleConfig {
    Present(GoogleClient),
    Missing(MissingGoogle),
}

/// Who Exo is to Google and to Microsoft, and where they are.
#[derive(Clone)]
pub struct ClientConfig {
    google: GoogleConfig,
    endpoints: Endpoints,
    microsoft: MicrosoftClient,
}

/// Leaves the secret out.
impl std::fmt::Debug for ClientConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientConfig")
            .field("client_id", &self.client_id())
            .field("endpoints", &self.endpoints)
            .field("microsoft", &self.microsoft)
            .finish_non_exhaustive()
    }
}

/// The parts of Google's client JSON Exo reads.
#[derive(Deserialize)]
struct File {
    installed: Option<Installed>,
    web: Option<serde::de::IgnoredAny>,
}

/// Leaves out its `auth_uri` and `token_uri`, which are always Google's, so a client file someone
/// shares can't send the browser or the code exchange elsewhere.
#[derive(Deserialize)]
struct Installed {
    client_id: String,
    client_secret: String,
}

impl ClientConfig {
    /// A Google client for `endpoints`, e.g. the mock servers'. Microsoft's is Exo's own, at
    /// Microsoft, unless [`with_microsoft`](Self::with_microsoft) says otherwise.
    pub fn new(
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
        endpoints: Endpoints,
    ) -> Self {
        Self {
            google: GoogleConfig::Present(GoogleClient {
                client_id: client_id.into(),
                client_secret: client_secret.into(),
            }),
            endpoints,
            microsoft: MicrosoftClient::default(),
        }
    }

    /// A configuration without a Google client file (reporting `path` / `from_env` if Google
    /// sign-in is attempted), using Exo's built-in Microsoft client unless
    /// [`with_microsoft`](Self::with_microsoft) overrides it.
    pub fn without_google(path: PathBuf, from_env: bool) -> Self {
        Self {
            google: GoogleConfig::Missing(MissingGoogle { path, from_env }),
            endpoints: Endpoints::google(),
            microsoft: MicrosoftClient::default(),
        }
    }

    /// The same configuration, with `microsoft` for Microsoft accounts.
    #[must_use]
    pub fn with_microsoft(self, microsoft: MicrosoftClient) -> Self {
        Self { microsoft, ..self }
    }

    /// Reads the client file named by `EXO_GOOGLE_OAUTH_CLIENT`, or else
    /// `google-oauth-client.json` in `config_dir` (Exo's config folder,
    /// `~/Library/Application Support/Exo` on macOS), and Microsoft's from the same folder
    /// ([`MicrosoftClient::load`]). Both talk to their production endpoints, whatever URLs the
    /// files list.
    ///
    /// A missing Google client file is allowed so Microsoft-only users can sign in with Exo's
    /// built-in Microsoft client; attempting Google sign-in then fails with
    /// [`Error::MissingClient`] naming where to put the file.
    ///
    /// # Errors
    /// [`Error::InvalidClient`] when an existing Google or Microsoft client file can't be read or
    /// isn't a client Exo can use.
    pub fn load(config_dir: &Path) -> Result<Self, Error> {
        let microsoft = MicrosoftClient::load(config_dir)?;
        let (path, from_env) = locate(std::env::var_os(ENV), config_dir);
        let config = match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).map_err(|reason| Error::InvalidClient {
                provider: crate::Provider::Google,
                path,
                reason,
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Self::without_google(path, from_env)
            }
            Err(error) => {
                return Err(Error::InvalidClient {
                    provider: crate::Provider::Google,
                    path,
                    reason: format!("can't be read: {error}"),
                });
            }
        };
        Ok(config.with_microsoft(microsoft))
    }

    /// Parses Google's client JSON, explaining what's wrong when it isn't a Desktop app client.
    fn parse(text: &str) -> Result<Self, String> {
        let file: File = serde_json::from_str(text)
            .map_err(|error| format!("isn't Google's client JSON: {error}"))?;
        let installed = match (file.installed, file.web) {
            (Some(installed), _) => installed,
            (None, Some(_)) => {
                return Err(
                    "is for a \"Web application\" client; Exo needs a \"Desktop app\" client"
                        .into(),
                );
            }
            (None, None) => return Err("has no \"installed\" (Desktop app) client".into()),
        };
        Ok(Self::new(
            installed.client_id,
            installed.client_secret,
            Endpoints::google(),
        ))
    }

    /// The Google client's ID, which Google shows on the consent screen's "app info", or `None`
    /// when no Google client file was loaded.
    #[must_use]
    pub fn client_id(&self) -> Option<&str> {
        match &self.google {
            GoogleConfig::Present(google) => Some(&google.client_id),
            GoogleConfig::Missing(_) => None,
        }
    }

    /// Whether a Google OAuth client was loaded.
    #[must_use]
    pub fn has_google(&self) -> bool {
        matches!(self.google, GoogleConfig::Present(_))
    }

    /// The Google APIs this client talks to.
    #[must_use]
    pub fn endpoints(&self) -> &Endpoints {
        &self.endpoints
    }

    /// Who Exo is to Microsoft.
    #[must_use]
    pub fn microsoft(&self) -> &MicrosoftClient {
        &self.microsoft
    }

    pub(crate) fn oauth(&self) -> Result<OAuthClient, MissingGoogle> {
        match &self.google {
            GoogleConfig::Present(google) => Ok(OAuthClient::new(
                self.endpoints.oauth.clone(),
                &google.client_id,
                Some(google.client_secret.clone()),
            )),
            GoogleConfig::Missing(missing) => Err(missing.clone()),
        }
    }
}

/// The client file to read, and whether the environment named it. An empty variable is unset.
fn locate(env: Option<OsString>, config_dir: &Path) -> (PathBuf, bool) {
    match env.filter(|path| !path.is_empty()) {
        Some(path) => (PathBuf::from(path), true),
        None => (config_dir.join(FILE_NAME), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape of Google's download, with made-up values.
    const DESKTOP: &str = r#"{"installed":{"client_id":"123-abc.apps.googleusercontent.com","project_id":"exo-dev","auth_uri":"https://accounts.google.com/o/oauth2/auth","token_uri":"https://oauth2.googleapis.com/token","auth_provider_x509_cert_url":"https://www.googleapis.com/oauth2/v1/certs","client_secret":"GOCSPX-not-a-real-secret","redirect_uris":["http://localhost"]}}"#;

    #[test]
    fn reads_a_desktop_client() {
        let config = ClientConfig::parse(DESKTOP).unwrap();
        assert_eq!(
            config.client_id(),
            Some("123-abc.apps.googleusercontent.com")
        );
        assert!(config.has_google());
        assert_eq!(config.endpoints(), &Endpoints::google());
    }

    #[test]
    fn a_file_cant_send_sign_in_elsewhere() {
        // A client file someone shared, pointing the code exchange (with its verifier) at
        // their server and the browser at a local file.
        let shared = DESKTOP
            .replace(
                "https://oauth2.googleapis.com/token",
                "https://collector.example/token",
            )
            .replace(
                "https://accounts.google.com/o/oauth2/auth",
                "file:///Users/someone/Downloads/run.command",
            );
        let config = ClientConfig::parse(&shared).unwrap();
        assert_eq!(config.endpoints(), &Endpoints::google());
    }

    #[test]
    fn debug_leaves_the_secret_out() {
        let config = ClientConfig::parse(DESKTOP).unwrap();
        assert!(!format!("{config:?}").contains("GOCSPX"));
        assert!(!format!("{:?}", config.oauth().unwrap()).contains("GOCSPX"));
    }

    #[test]
    fn explains_wrong_files() {
        let web = r#"{"web":{"client_id":"x","client_secret":"y"}}"#;
        assert!(
            ClientConfig::parse(web)
                .unwrap_err()
                .contains("\"Desktop app\"")
        );
        assert!(
            ClientConfig::parse("{}")
                .unwrap_err()
                .contains("no \"installed\"")
        );
        assert!(
            ClientConfig::parse("not json")
                .unwrap_err()
                .contains("isn't Google's client JSON")
        );
    }

    #[test]
    fn the_environment_wins_over_the_config_folder() {
        let dir = Path::new("/config");
        assert_eq!(
            locate(Some("/elsewhere/client.json".into()), dir),
            (PathBuf::from("/elsewhere/client.json"), true)
        );
        assert_eq!(locate(None, dir), (dir.join(FILE_NAME), false));
        assert_eq!(
            locate(Some(OsString::new()), dir),
            (dir.join(FILE_NAME), false)
        );
    }

    #[test]
    fn missing_google_file_loads_with_builtin_microsoft_and_remembers_where_to_put_one() {
        let dir = tempfile::tempdir().unwrap();
        let config = ClientConfig::load(dir.path()).unwrap();
        assert_eq!(config.microsoft().client_id(), crate::EXO_CLIENT_ID);
        let Err(missing) = config.oauth() else {
            // Unless the environment names a Google client file, which a developer's shell might.
            return;
        };
        if missing.from_env {
            return;
        }
        assert!(!config.has_google());
        assert_eq!(config.client_id(), None);
        let message = missing.to_error().to_string();
        assert!(
            message.contains(&dir.path().join(FILE_NAME).display().to_string()),
            "{message}"
        );
        assert!(
            message.contains("Desktop app") && message.contains(ENV),
            "{message}"
        );
    }

    #[test]
    fn broken_microsoft_override_still_fails_loudly_without_google_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(crate::microsoft::FILE_NAME);
        std::fs::write(&file, "{ not json").unwrap();
        let error = ClientConfig::load(dir.path()).unwrap_err();
        assert!(
            matches!(
                &error,
                Error::InvalidClient {
                    provider: crate::Provider::Microsoft,
                    path,
                    ..
                } if *path == file
            ),
            "{error}"
        );
    }

    /// Parses the real client file without printing it. That's the file `EXO_GOOGLE_OAUTH_CLIENT`
    /// names, or the config folder's. Run it with
    /// `EXO_GOOGLE_OAUTH_CLIENT=… cargo test -p exo-auth -- --ignored real_client`.
    #[test]
    #[ignore = "reads a real client file"]
    fn real_client_file_parses() {
        let config =
            ClientConfig::load(Path::new("/nonexistent")).map_err(|error| error.to_string());
        let config = config.expect("the client file parses");
        assert!(
            config
                .client_id()
                .is_some_and(|id| id.ends_with(".apps.googleusercontent.com"))
        );
        assert_eq!(
            config.endpoints().oauth.token.host_str(),
            Some("oauth2.googleapis.com")
        );
    }
}
