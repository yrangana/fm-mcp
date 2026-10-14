# Records docs/demo/setup.gif: brew install, fm-mcp install, fm-mcp doctor.
# Run from the repo root: vhs docs/demo/setup.tape
# Before recording, remove fm-mcp so the install is real (see docs/demo/README.md).

Output docs/demo/setup.gif

Set Shell "zsh"
Set FontSize 16
Set Width 1280
Set Height 680
Set Padding 24
Set Theme "Builtin Dark"
Set TypingSpeed 45ms
# `Wait` waits for this prompt to come back, so the next command isn't typed
# while the last one is still running.
Set WaitPattern /\$$/

# Hidden setup:
# - a plain prompt, and no Homebrew auto-update or hints;
# - brew's warnings (stderr) are dropped, because they come from the
#   recording Mac's other taps, not from fm-mcp;
# - fm-mcp's output shows the home folder as ~, so the user name stays out
#   of the GIF.
Hide
Type `export PS1='$ ' HOMEBREW_NO_AUTO_UPDATE=1 HOMEBREW_NO_ENV_HINTS=1`
Enter
Type `brew() { command brew "$@" 2>/dev/null; }`
Enter
Type `fm-mcp() { command fm-mcp "$@" 2>&1 | sed "s|$HOME|~|g"; }`
Enter
Type "clear"
Enter
Sleep 500ms
Show

Type "brew install yrangana/tap/fm-mcp"
Enter
Wait@90s
Sleep 2s

Hide
Type "clear"
Enter
Sleep 300ms
Show

Type "fm-mcp install"
Enter
Wait@60s
Sleep 3s

Hide
Type "clear"
Enter
Sleep 300ms
Show

Type "fm-mcp doctor"
Enter
Wait+Screen@60s /All checks passed/
Sleep 4s
