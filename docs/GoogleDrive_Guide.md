# Guide to Connecting AeroFTP to Google Drive

This guide explains step by step how to configure AeroFTP to connect to Google Drive using your own OAuth credentials. The procedure is based on rclone's documentation, but adapted for AeroFTP's graphical interface.

## Prerequisites

- A Google account with access to Google Cloud Console
- AeroFTP installed on your system
- A web browser for OAuth authorization

## Step 1: Creating the Google App in Cloud Console

### 1.1 Access Google Cloud Console

1. Go to [Google Cloud Console](https://console.cloud.google.com/)
2. Sign in with your Google account
3. If you don't have a project yet, create a new one:
   - Click on "Select a project" in the top left
   - Click on "New project"
   - Give the project a name (e.g. "AeroFTP-Drive")
   - Select organization if applicable
   - Click "Create"

### 1.2 Enable Google Drive API

1. In the console, go to "APIs & Services" → "Library"
2. Search for "Google Drive API"
3. Select "Google Drive API" from the results
4. Click "Enable"

### 1.3 Configure OAuth Consent Screen

1. Go to "APIs & Services" → "OAuth consent screen"
2. Choose the user type: "External" (for personal use) or "Internal" (for Google Workspace)
3. Fill in the app information:
   - **App name**: AeroFTP
   - **User support email**: your email address
   - **Authorized domains**: leave empty for personal use
   - **Developer contact information**: your email address
4. In the "Scopes" section, add the scope for Google Drive:
   - Click "ADD OR REMOVE SCOPES"
   - Search and select ".../auth/drive" (full access) or ".../auth/drive.file" (only files created by AeroFTP)
5. Save and continue

### 1.4 Create OAuth Credentials

1. Go to "APIs & Services" → "Credentials"
2. Click "Create credentials" → "OAuth client ID"
3. Select "Web application" as type
4. In the "Authorized redirect URIs" section, add:
   - `http://127.0.0.1` (AeroFTP handles the port automatically)
5. Click "Create"
6. **IMPORTANT**: Copy and save the **Client ID** and **Client Secret** that are displayed. You'll use them in AeroFTP.

## Step 2: Configuring AeroFTP

### 2.1 Open OAuth Settings

1. Launch AeroFTP
2. Go to application settings (usually via menu or settings icon)
3. Look for the "OAuth" or "Providers" section
4. Select "Google API" or "Google Drive"

### 2.2 Enter Credentials

1. In the "Client ID" field, paste the Client ID obtained from Google Cloud Console
2. In the "Client Secret" field, paste the Client Secret
3. Some fields may have placeholders like:
   - Client ID: `xxxxxxxx.apps.googleusercontent.com`
   - Client Secret: `GOCSPX-...`
4. Verify the values are correct
5. Save the settings

### 2.3 Initial Authorization

1. The first time you use Google Drive with AeroFTP, the application will automatically open your web browser
2. Sign in with your Google account if necessary
3. In the consent screen, click "Allow" to authorize AeroFTP to access Google Drive
4. The browser may show a confirmation message or redirect to a local page

## Step 3: Testing the Connection

### 3.1 Verify Connection

Use AeroFTP commands to test the connection:

```bash
# List saved profiles
aeroftp-cli profiles --json

# Test connection to Google Drive (replace "MyDrive" with your profile name)
aeroftp-cli connect --profile "MyDrive"

# List files in Google Drive root
aeroftp-cli ls --profile "MyDrive" /

# Get profile information and quota
aeroftp-cli about --profile "MyDrive" --json
```

### 3.2 Examples of Common Operations

```bash
# Download a file
aeroftp-cli get --profile "MyDrive" /remote/path/file.txt ./local/file.txt

# Upload a file
aeroftp-cli put --profile "MyDrive" ./local/file.txt /remote/path/file.txt

# Sync a folder
aeroftp-cli sync --profile "MyDrive" ./local/folder /remote/folder --dry-run

# Create a directory
aeroftp-cli mkdir --profile "MyDrive" /new_folder
```

## Security and Best Practices

### Credential Management
- AeroFTP stores credentials in an encrypted vault (AES-256-GCM)
- Credentials are never exposed in CLI commands or logs
- Always use named profiles instead of inserting credentials manually

### Access Scopes
- Choose the appropriate scope based on your needs:
  - `drive`: Full access to all files
  - `drive.file`: Only files created by AeroFTP
  - `drive.readonly`: Read-only access

### Sharing and Security
- Credentials are isolated per profile
- You can have multiple Google Drive configurations with different accounts
- Permissions can be revoked from the [Google Account settings](https://myaccount.google.com/permissions)

## Troubleshooting

### Error: "invalid_client"
- Verify Client ID and Client Secret are correct
- Ensure the OAuth app is configured for "Web application"

### Error: "redirect_uri_mismatch"
- Verify that `http://127.0.0.1` is in the authorized redirect URIs
- Make sure AeroFTP is running when attempting authorization

### Error: "access_denied"
- Verify you clicked "Allow" in the consent screen
- Check that the Google account has access to Google Drive

### Slow connection or timeout
- Verify internet connection
- Some providers may have speed limits
- Use `--fast-list` for faster listings (if supported)

### Issues with large files
- Google Drive has upload limits (750GB/day)
- Use `--chunk-size` to optimize uploads

## Differences from rclone

While rclone requires manual CLI configuration, AeroFTP offers:

- **Graphical Interface**: Configuration via intuitive GUI
- **Enhanced Security**: Encrypted vault instead of configuration files
- **Automation**: Automatic OAuth authorization without manual intervention
- **Integration**: Native support for multiple cloud providers

## Additional Resources

- [Official Google Drive API documentation](https://developers.google.com/drive/api/v3/quickstart)
- [rclone Google Drive guide](https://rclone.org/drive/)
- [AeroFTP documentation](https://docs.aeroftp.app/)

---

*This guide is created for AeroFTP v3.5.3. Procedures may vary slightly with future versions.*