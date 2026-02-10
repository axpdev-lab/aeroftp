#!/usr/bin/env bash
#
# AeroCloud Shell Integration Installer
# Installs Nautilus/Nemo Python extensions and SVG emblems for sync status badges
#

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EMBLEM_DIR="$HOME/.local/share/icons/hicolor/scalable/emblems"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

print_info() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

print_success() {
    echo -e "${GREEN}[OK]${NC} $1"
}

print_warning() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

print_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

detect_file_managers() {
    local managers=()

    if command -v nautilus &> /dev/null; then
        managers+=("nautilus")
    fi

    if command -v nemo &> /dev/null; then
        managers+=("nemo")
    fi

    if command -v thunar &> /dev/null; then
        # Thunar doesn't support Python extensions, but we detect it for future reference
        print_warning "Thunar detected but Python extensions not supported (future: UCA integration)"
    fi

    echo "${managers[@]}"
}

install_nautilus_extension() {
    print_info "Installing Nautilus extension..."

    local extension_dir="$HOME/.local/share/nautilus-python/extensions"
    mkdir -p "$extension_dir"

    cp "$SCRIPT_DIR/nautilus/aerocloud_nautilus.py" "$extension_dir/"
    chmod +x "$extension_dir/aerocloud_nautilus.py"

    print_success "Nautilus extension installed to $extension_dir"
}

install_nemo_extension() {
    print_info "Installing Nemo extension..."

    local extension_dir="$HOME/.local/share/nemo-python/extensions"
    mkdir -p "$extension_dir"

    cp "$SCRIPT_DIR/nemo/aerocloud_nemo.py" "$extension_dir/"
    chmod +x "$extension_dir/aerocloud_nemo.py"

    print_success "Nemo extension installed to $extension_dir"
}

install_emblems() {
    print_info "Installing SVG emblem icons..."

    mkdir -p "$EMBLEM_DIR"

    local emblem_count=0
    for emblem in "$SCRIPT_DIR/emblems"/*.svg; do
        if [[ -f "$emblem" ]]; then
            cp "$emblem" "$EMBLEM_DIR/"
            ((emblem_count++))
        fi
    done

    if [[ $emblem_count -gt 0 ]]; then
        # Update icon cache
        if command -v gtk-update-icon-cache &> /dev/null; then
            gtk-update-icon-cache -f -t "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
        fi
        print_success "Installed $emblem_count emblem icons to $EMBLEM_DIR"
    else
        print_warning "No emblem SVG files found in $SCRIPT_DIR/emblems"
    fi
}

reload_file_managers() {
    local managers=("$@")

    print_info "Reloading file managers..."

    for manager in "${managers[@]}"; do
        case "$manager" in
            nautilus)
                if pgrep -x nautilus > /dev/null; then
                    nautilus -q 2>/dev/null || true
                    print_success "Nautilus reloaded"
                else
                    print_info "Nautilus not running, extension will load on next start"
                fi
                ;;
            nemo)
                if pgrep -x nemo > /dev/null; then
                    nemo -q 2>/dev/null || true
                    print_success "Nemo reloaded"
                else
                    print_info "Nemo not running, extension will load on next start"
                fi
                ;;
        esac
    done
}

uninstall_extensions() {
    print_info "Uninstalling AeroCloud shell integration..."

    local uninstalled=0

    # Remove Nautilus extension
    if [[ -f "$HOME/.local/share/nautilus-python/extensions/aerocloud_nautilus.py" ]]; then
        rm -f "$HOME/.local/share/nautilus-python/extensions/aerocloud_nautilus.py"
        print_success "Removed Nautilus extension"
        ((uninstalled++))
    fi

    # Remove Nemo extension
    if [[ -f "$HOME/.local/share/nemo-python/extensions/aerocloud_nemo.py" ]]; then
        rm -f "$HOME/.local/share/nemo-python/extensions/aerocloud_nemo.py"
        print_success "Removed Nemo extension"
        ((uninstalled++))
    fi

    # Remove emblems
    local emblem_count=0
    for emblem in "$EMBLEM_DIR"/emblem-aerocloud-*.svg; do
        if [[ -f "$emblem" ]]; then
            rm -f "$emblem"
            ((emblem_count++))
        fi
    done

    if [[ $emblem_count -gt 0 ]]; then
        # Update icon cache
        if command -v gtk-update-icon-cache &> /dev/null; then
            gtk-update-icon-cache -f -t "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
        fi
        print_success "Removed $emblem_count emblem icons"
        ((uninstalled++))
    fi

    if [[ $uninstalled -eq 0 ]]; then
        print_warning "No AeroCloud shell integration found"
    else
        print_success "Uninstallation complete"
    fi

    # Reload file managers
    local managers=($(detect_file_managers))
    if [[ ${#managers[@]} -gt 0 ]]; then
        reload_file_managers "${managers[@]}"
    fi
}

check_dependencies() {
    local missing_deps=()

    # Check for Python 3
    if ! command -v python3 &> /dev/null; then
        missing_deps+=("python3")
    fi

    # Check for python3-gi (GObject introspection)
    if ! python3 -c "import gi" 2>/dev/null; then
        missing_deps+=("python3-gi")
    fi

    if [[ ${#missing_deps[@]} -gt 0 ]]; then
        print_error "Missing dependencies: ${missing_deps[*]}"
        print_info "Install with: sudo apt install ${missing_deps[*]}"
        return 1
    fi

    return 0
}

main() {
    echo "=========================================="
    echo "  AeroCloud Shell Integration Installer"
    echo "=========================================="
    echo

    # Handle uninstall flag
    if [[ "$1" == "--uninstall" || "$1" == "-u" ]]; then
        uninstall_extensions
        exit 0
    fi

    # Check dependencies
    if ! check_dependencies; then
        exit 1
    fi

    # Detect installed file managers
    local managers=($(detect_file_managers))

    if [[ ${#managers[@]} -eq 0 ]]; then
        print_warning "No supported file managers detected (Nautilus, Nemo)"
        print_info "Emblems will still be installed for future use"
    else
        print_info "Detected file managers: ${managers[*]}"
    fi

    # Install components
    for manager in "${managers[@]}"; do
        case "$manager" in
            nautilus)
                install_nautilus_extension
                ;;
            nemo)
                install_nemo_extension
                ;;
        esac
    done

    install_emblems

    # Reload file managers
    if [[ ${#managers[@]} -gt 0 ]]; then
        reload_file_managers "${managers[@]}"
    fi

    echo
    print_success "Installation complete!"
    echo
    print_info "Notes:"
    echo "  - Extensions will show sync status emblems on files"
    echo "  - Requires AeroCloud badge daemon to be running"
    echo "  - To uninstall: $0 --uninstall"
    echo
}

main "$@"
