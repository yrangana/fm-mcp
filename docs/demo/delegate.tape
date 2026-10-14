# Records docs/demo/delegate.gif: Claude Code hands a log to fm-mcp's summarise.
# Run from the repo root: vhs docs/demo/delegate.tape
# Needs the demo folder set up first (see docs/demo/README.md).

Output docs/demo/delegate.gif

Set Shell "zsh"
Set FontSize 16
Set Width 1200
Set Height 720
Set Padding 20
Set Theme "Builtin Dark"
Set TypingSpeed 40ms

# Start in the demo folder with a plain prompt. Default permission mode keeps
# "auto mode on" out of the footer, so summarise and the read-only tools Claude
# uses to check the summary are allowed up front: no prompt mid-recording.
# CLAUDE_CODE_CHILD_SESSION is cleared in case vhs runs inside Claude Code,
# which would otherwise show a "transcript saving is off" warning.
# The folder is in /Users/Shared, not ~, so paths Claude prints don't show the
# user name, and an empty status line hides the one in a custom status line.
# If the folder is missing the shell exits, so the take fails instead of
# recording Claude in the wrong folder.
Hide
Type "unset CLAUDE_CODE_CHILD_SESSION; export PS1='$ '; cd /Users/Shared/fm-mcp-demo || exit; clear"
Enter
Sleep 500ms
Type `claude --permission-mode default --allowedTools mcp__fm-mcp__summarise Read Grep Glob 'Bash(grep:*)' 'Bash(wc:*)' --settings '{"statusLine":{"type":"command","command":"true"}}'`
Enter
Sleep 5s
Show

Type "What went wrong in checkout.log? Have fm-mcp summarise it, in a short paragraph."
Sleep 800ms
Enter

# The agent calls summarise (about 5 s on the model), then answers.
Wait+Screen@90s /(?i)inventory|SKU 88-1043/
Sleep 6s
