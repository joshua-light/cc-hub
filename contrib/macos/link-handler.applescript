-- cc-hub Link: the macOS side of cc-hub:// deep links.
--
-- install-link-handler.sh compiles this into "cc-hub Link.app" and declares
-- the `cc-hub` URL scheme in its Info.plist. macOS then launches the app for
-- any cc-hub:// URL and delivers the URL to `open location`; the app hands it
-- to `cc-hub open` and quits. `cc-hub open` may wait up to two minutes for
-- the new session to become idle before pasting the prompt, so it runs
-- detached — the app must not sit on screen that long.
--
-- __CC_HUB__ is the absolute path of the cc-hub binary, filled in at install.
-- PATH is widened because a launched app inherits launchd's minimal one, and
-- cc-hub needs `tmux` (Homebrew) plus whatever the agent command resolves to.

on open location theUrl
	set logDir to "$HOME/Library/Caches/cc-hub"
	set cmd to "export PATH=/opt/homebrew/bin:/usr/local/bin:$HOME/.local/bin:$PATH; export SHELL=${SHELL:-/bin/zsh}; mkdir -p " & logDir & "; nohup " & quoted form of "__CC_HUB__" & " open " & quoted form of theUrl & " >> " & logDir & "/link.log 2>&1 &"
	do shell script cmd
	quit
end open location
