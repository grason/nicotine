#!/bin/bash
# Nicotine - One-line installer
# Usage: curl -sSL https://raw.githubusercontent.com/grason/nicotine/master/install-github.sh | bash

set -e

REPO="grason/nicotine"
INSTALL_DIR="$HOME/.local/bin"
BINARY_NAME="nicotine"

echo "=== Nicotine Installer ==="
echo

# Detect architecture
ARCH=$(uname -m)
case $ARCH in
x86_64)
  ARCH="x86_64"
  ;;
aarch64 | arm64)
  ARCH="aarch64"
  ;;
*)
  echo "Unsupported architecture: $ARCH"
  exit 1
  ;;
esac

echo "[1/5] Detecting latest release..."
RELEASE_INFO=$(curl -sL "https://api.github.com/repos/$REPO/releases/latest")
BINARY_URL=$(echo "$RELEASE_INFO" | grep "browser_download_url.*nicotine-linux-$ARCH\"" | cut -d '"' -f 4)
ICON_URL=$(echo "$RELEASE_INFO" | grep "browser_download_url.*icon.png\"" | cut -d '"' -f 4)
DESKTOP_URL=$(echo "$RELEASE_INFO" | grep "browser_download_url.*nicotine.desktop\"" | cut -d '"' -f 4)

if [ -z "$BINARY_URL" ]; then
  echo "Error: Could not find release for linux-$ARCH"
  exit 1
fi

echo "[2/5] Downloading nicotine..."
mkdir -p "$INSTALL_DIR"
curl -sL "$BINARY_URL" -o "/tmp/$BINARY_NAME"
chmod +x "/tmp/$BINARY_NAME"
mv "/tmp/$BINARY_NAME" "$INSTALL_DIR/"

echo "[3/5] Installing desktop file and icon..."
mkdir -p ~/.local/share/applications
mkdir -p ~/.local/share/icons/hicolor/256x256/apps
if [ -n "$ICON_URL" ]; then
  curl -sL "$ICON_URL" -o ~/.local/share/icons/hicolor/256x256/apps/nicotine.png
fi
if [ -n "$DESKTOP_URL" ]; then
  curl -sL "$DESKTOP_URL" -o ~/.local/share/applications/nicotine.desktop
fi
update-desktop-database ~/.local/share/applications 2>/dev/null || true
gtk-update-icon-cache ~/.local/share/icons/hicolor 2>/dev/null || true

# Add to PATH if needed
if [[ ":$PATH:" != *":$INSTALL_DIR:"* ]]; then
  echo "[4/5] Adding $INSTALL_DIR to PATH..."
  SHELL_RC=""
  if [ -n "$BASH_VERSION" ]; then
    SHELL_RC="$HOME/.bashrc"
  elif [ -n "$ZSH_VERSION" ]; then
    SHELL_RC="$HOME/.zshrc"
  fi

  if [ -n "$SHELL_RC" ] && [ -f "$SHELL_RC" ]; then
    if ! grep -q "export PATH.*$INSTALL_DIR" "$SHELL_RC" 2>/dev/null; then
      echo "" >>"$SHELL_RC"
      echo "# Nicotine" >>"$SHELL_RC"
      echo "export PATH=\"\$HOME/.local/bin:\$PATH\"" >>"$SHELL_RC"
      echo "Added to $SHELL_RC"
    fi
  fi
else
  echo "[4/5] PATH already configured"
fi

echo "[5/5] Done!"
echo
echo "✓ Installation complete!"
echo
echo "Quick start:"
echo "  nicotine start"
echo
echo "Config will be auto-generated at: ~/.config/nicotine/config.toml"
echo
echo "Note: Restart your terminal first if PATH was just updated"
