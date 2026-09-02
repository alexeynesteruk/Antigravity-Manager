#!/bin/bash

# Script to close PRs already integrated into v4.0.3
# Before use, make sure GitHub CLI is installed and logged in: brew install gh && gh auth login

REPO="lbjlaq/Antigravity-Manager"
VERSION="v4.0.3"

# Thank-you message template
THANK_YOU_MESSAGE="感谢您的贡献！🎉

此 PR 的更改已被手动集成到 ${VERSION} 版本中。

相关更新已包含在以下文件中：
- README.md 的版本更新日志
- 贡献者列表

再次感谢您对 Antigravity Tools 项目的支持！

---

Thank you for your contribution! 🎉

The changes from this PR have been manually integrated into ${VERSION}.

The updates are documented in:
- README.md changelog
- Contributors list

Thank you again for your support of the Antigravity Tools project!"

echo "================================================"
echo "Closing PRs already integrated into ${VERSION}"
echo "================================================"
echo ""

# PR list: format is "PR number|author|title"
PRS_LIST=(
    "825|IamAshrafee|[Internationalization] Device Fingerprint Dialog localization"
    "822|Koshikai|[Japanese] Add missing translations and refine terminology",
    "798|vietnhatthai|[Translation Fix] Correct spelling error in Vietnamese settings",
    "846|lengjingxu|[Core Feature] Client hot update and token statistics system",
    "949|lbjlaq|Streaming chunks order fix",
    "950|lbjlaq|[Fix] Remove redundant code and update README",
    "973|Mag1cFall|fix: fix Windows platform startup arguments not taking effect"
)

# Check whether GitHub CLI is installed
if ! command -v gh &> /dev/null; then
    echo "❌ GitHub CLI is not installed"
    echo ""
    echo "Please install GitHub CLI first:"
    echo "  brew install gh"
    echo ""
    echo "Then log in:"
    echo "  gh auth login"
    echo ""
    exit 1
fi

# Check whether already logged in
if ! gh auth status &> /dev/null; then
    echo "❌ Not logged in to GitHub CLI"
    echo ""
    echo "Please log in first:"
    echo "  gh auth login"
    echo ""
    exit 1
fi

echo "✅ GitHub CLI is ready"
echo ""

# Iterate over and process each PR
for item in "${PRS_LIST[@]}"; do
    PR_NUM=$(echo "$item" | cut -d'|' -f1)
    AUTHOR=$(echo "$item" | cut -d'|' -f2)
    TITLE=$(echo "$item" | cut -d'|' -f3)

    echo "----------------------------------------"
    echo "Processing PR #${PR_NUM}: ${TITLE}"
    echo "Author: @${AUTHOR}"
    echo "----------------------------------------"

    # Add the thank-you comment
    echo "📝 Adding thank-you comment..."
    gh pr comment ${PR_NUM} --repo ${REPO} --body "${THANK_YOU_MESSAGE}"

    if [ $? -eq 0 ]; then
        echo "✅ Comment added"
    else
        echo "❌ Failed to add comment"
        continue
    fi

    # Close the PR
    echo "🔒 Closing PR..."
    gh pr close ${PR_NUM} --repo ${REPO} --comment "Integrated into ${VERSION}; closing this PR."

    if [ $? -eq 0 ]; then
        echo "✅ PR #${PR_NUM} closed"
    else
        echo "❌ Failed to close PR #${PR_NUM}"
    fi

    echo ""
    sleep 2  # Avoid API rate limiting
done

echo "================================================"
echo "✅ All PRs processed!"
echo "================================================"
echo ""
echo "Visit the following link to see the results:"
echo "https://github.com/${REPO}/pulls?q=is%3Apr+is%3Aclosed"
