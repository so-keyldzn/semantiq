# Publishing Semantiq

## Prerequisites

### 1. GitHub Repository
Create a public repository at `github.com/so-keyldzn/semantiq`

### 2. crates.io Account
1. Create account at https://crates.io
2. Get API token from https://crates.io/settings/tokens
3. Add as GitHub secret: `CARGO_TOKEN`

### 3. npm Account
1. Create account at https://npmjs.com
2. Get access token from https://www.npmjs.com/settings/tokens
3. Add as GitHub secret: `NPM_TOKEN`

### 4. Homebrew Tap Repository
Create a repository at `github.com/so-keyldzn/homebrew-tap`

## Release Process

### Automatic (Recommended)

1. Update version in `Cargo.toml`:
   ```toml
   version = "0.1.0"
   ```

2. Update version in `npm/package.json`:
   ```json
   "version": "0.1.0"
   ```

3. Commit and tag:
   ```bash
   git add -A
   git commit -m "Release v0.1.0"
   git tag v0.1.0
   git push origin main --tags
   ```

4. GitHub Actions will automatically:
   - Build binaries for all platforms
   - Create GitHub Release
   - Publish to crates.io
   - Publish to npm

5. Update Homebrew formula manually:
   - Download the release archives
   - Calculate SHA256: `shasum -a 256 semantiq-v0.1.0-*.tar.gz`
   - Update `homebrew/Formula/semantiq.rb` with real SHA256 values
   - Push to `so-keyldzn/homebrew-tap` repository

### Manual Publishing

#### crates.io
```bash
cd crates/semantiq
cargo publish
```

#### npm
```bash
cd npm
npm publish --access public
```

#### Homebrew
1. Build release binaries
2. Upload to GitHub Releases
3. Update formula SHA256 values
4. Push to homebrew-tap repo

## User Installation

After publishing, users can install with:

```bash
# Homebrew (macOS/Linux)
brew tap so-keyldzn/tap
brew install semantiq

# npm (cross-platform)
npm i -g semantiq

# Cargo (Rust users)
cargo install semantiq
```
