#!/bin/bash
# Automated release script for CCC
# Usage: ./scripts/release.sh [major|minor|patch]

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

info()    { echo -e "${BLUE}[release] $1${NC}"; }
success() { echo -e "${GREEN}[release] ✓ $1${NC}"; }
warn()    { echo -e "${YELLOW}[release] ⚠ $1${NC}"; }
error()   { echo -e "${RED}[release] ✗ $1${NC}"; exit 1; }

check_prerequisites() {
    info "Checking prerequisites..."

    command -v gh  >/dev/null 2>&1 || error "gh CLI not installed (brew install gh)"
    gh auth status >/dev/null 2>&1 || error "gh CLI not authenticated (gh auth login)"

    if [[ -n $(git status --porcelain) ]]; then
        error "Working directory is not clean. Commit or stash changes first."
    fi

    CURRENT_BRANCH=$(git rev-parse --abbrev-ref HEAD)
    [[ "$CURRENT_BRANCH" == "main" ]] || error "Not on main branch (on: $CURRENT_BRANCH)"

    success "Prerequisites OK"
}

get_current_version() {
    git tag -l 'v*' | sort -V | tail -1 | sed 's/^v//'
}

calculate_next_version() {
    local current=$1 bump_type=$2
    IFS='.' read -r major minor patch <<< "$current"
    case $bump_type in
        major) major=$((major + 1)); minor=0; patch=0 ;;
        minor) minor=$((minor + 1)); patch=0 ;;
        patch) patch=$((patch + 1)) ;;
        *) error "Invalid bump type: $bump_type (use major, minor, or patch)" ;;
    esac
    echo "$major.$minor.$patch"
}

generate_changelog_entry() {
    local prev_version=$1 new_version=$2
    local date; date=$(date +%Y-%m-%d)

    info "Generating changelog from v$prev_version to HEAD..." >&2

    local commits
    if git rev-parse "v$prev_version" &>/dev/null; then
        commits=$(git log "v$prev_version"..HEAD --pretty=format:"%h %s" --no-merges)
    else
        commits=$(git log --pretty=format:"%h %s" --no-merges)
    fi

    local added="" changed="" fixed="" removed="" other=""

    while IFS= read -r line; do
        if [[ $line =~ ^[a-f0-9]+\ feat(\(.*\))?:\ (.*) ]];     then added+="- ${BASH_REMATCH[2]}\n"
        elif [[ $line =~ ^[a-f0-9]+\ fix(\(.*\))?:\ (.*) ]];    then fixed+="- ${BASH_REMATCH[2]}\n"
        elif [[ $line =~ ^[a-f0-9]+\ refactor(\(.*\))?:\ (.*) ]]; then changed+="- ${BASH_REMATCH[2]}\n"
        elif [[ $line =~ ^[a-f0-9]+\ (chore|docs)(\(.*\))?:\ (.*) ]]; then other+="- ${BASH_REMATCH[3]}\n"
        else other+="- $(echo "$line" | cut -d' ' -f2-)\n"
        fi
    done <<< "$commits"

    local entry="## [$new_version] - $date\n\n"
    [[ -n "$added"   ]] && entry+="### Added\n$added\n"
    [[ -n "$changed" ]] && entry+="### Changed\n$changed\n"
    [[ -n "$fixed"   ]] && entry+="### Fixed\n$fixed\n"
    [[ -n "$removed" ]] && entry+="### Removed\n$removed\n"

    echo -e "$entry"
}

update_changelog() {
    local changelog_entry=$1
    local changelog_file="CHANGELOG.md"

    info "Updating $changelog_file..."
    [[ -f "$changelog_file" ]] || error "CHANGELOG.md not found"

    local temp_file entry_file
    temp_file=$(mktemp)
    entry_file=$(mktemp)

    echo -e "$changelog_entry" > "$entry_file"

    awk '
        /^## \[Unreleased\]/ {
            print $0
            print ""
            while ((getline line < "'"$entry_file"'") > 0) print line
            close("'"$entry_file"'")
            next
        }
        { print }
    ' "$changelog_file" > "$temp_file"

    mv "$temp_file" "$changelog_file"
    rm "$entry_file"

    success "CHANGELOG.md updated"
}

run_tests() {
    info "Running tests..."
    local output_file
    output_file=$(mktemp)

    if ! cargo test --manifest-path Cargo.toml --quiet > "$output_file" 2>&1; then
        cat "$output_file"
        rm -f "$output_file"
        error "Tests failed. Fix before releasing."
    fi

    local test_status
    test_status=$(grep -E "test result|^test " "$output_file" | tail -3 | tr '\n' ' ' || echo "passed")
    rm -f "$output_file"
    success "Tests passed"
    echo "$test_status"
}

create_release() {
    local version=$1 prev_version=$2 test_status=$3

    info "Creating release v$version..."

    local release_notes commit_count
    if git rev-parse "v$prev_version" &>/dev/null; then
        release_notes=$(git log "v$prev_version"..HEAD --pretty=format:"- %s" --no-merges)
        commit_count=$(git rev-list --count "v$prev_version"..HEAD)
    else
        release_notes=$(git log --pretty=format:"- %s" --no-merges)
        commit_count=$(git rev-list --count HEAD)
    fi

    local repo_url
    repo_url=$(gh repo view --json url -q .url 2>/dev/null || echo "")

    cat > /tmp/ccc_release_notes.md << EOF
## CCC v$version

### Statistics
- **Commits since v$prev_version**: $commit_count
- **Test Status**: $test_status

### Changes

$release_notes
EOF

    if [[ -n "$repo_url" ]]; then
        printf '\n---\n\n**Full Changelog**: %s/compare/v%s...v%s\n' \
            "$repo_url" "$prev_version" "$version" >> /tmp/ccc_release_notes.md
    fi

    info "Committing CHANGELOG.md..."
    git add CHANGELOG.md
    if ! git diff --cached --quiet; then
        git commit -m "$(cat <<EOF
docs: update CHANGELOG for v$version

Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>
EOF
)"
    fi

    info "Syncing with origin..."
    git pull --rebase origin main

    info "Tagging v$version..."
    git tag -a "v$version" -m "Release v$version"

    info "Pushing..."
    git push origin main
    git push origin "v$version"

    info "Creating GitHub release..."
    gh release create "v$version" \
        --title "v$version" \
        --notes-file /tmp/ccc_release_notes.md

    rm /tmp/ccc_release_notes.md
    success "Release v$version created"
}

main() {
    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "  CCC Automated Release"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo ""

    check_prerequisites

    CURRENT_VERSION=$(get_current_version)
    if [[ -z "$CURRENT_VERSION" ]]; then
        CURRENT_VERSION="0.0.0"
        info "No prior semver tags — first release will be v0.1.0"
    else
        info "Current version: v$CURRENT_VERSION"
    fi

    BUMP_TYPE=${1:-patch}
    [[ "$BUMP_TYPE" =~ ^(major|minor|patch)$ ]] || \
        error "Invalid bump type: $BUMP_TYPE (use major, minor, or patch)"

    NEXT_VERSION=$(calculate_next_version "$CURRENT_VERSION" "$BUMP_TYPE")

    echo ""
    echo "  Current : v$CURRENT_VERSION"
    echo "  Next    : v$NEXT_VERSION  ($BUMP_TYPE)"
    echo ""

    CHANGELOG_ENTRY=$(generate_changelog_entry "$CURRENT_VERSION" "$NEXT_VERSION")

    info "Generated changelog entry:"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo -e "$CHANGELOG_ENTRY"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

    update_changelog "$CHANGELOG_ENTRY"

    TEST_STATUS=$(run_tests)

    create_release "$NEXT_VERSION" "$CURRENT_VERSION" "$TEST_STATUS"

    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "  Release complete: v$NEXT_VERSION"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo ""
}

main "$@"
