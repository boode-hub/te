te - the text editor for huge files
====================================

Run:        double-click te.exe (no installer, no admin rights)
Open:       File > Open, drop a file on the window, or "Open with > te" in Explorer
Update:     Help > Check for updates. Only releases signed with te's key are installed;
            the previous version is kept as te.old.exe until the new one has started.
Settings:   te.cfg next to te.exe (created on first run); saved looks go in profiles\
Own fonts:  put .ttf / .otf files in a fonts\ folder next to te.exe
Crashes:    if anything goes wrong, details are written to te-crash.log next to te.exe
Uninstall:  delete this folder

"Windows protected your PC"? te isn't code-signed yet: click More info, then Run anyway.

Shortcuts:  Ctrl+O open, Ctrl+S save, Ctrl+F find, Ctrl+H replace, Ctrl+G go to line,
            Ctrl+Shift+F search open tabs or folder, Ctrl+\ split view, Ctrl+, appearance.
            Right-click the text for Copy, Paste and every tool.

Command line:
  te.exe [file ...]       open files (or a folder)
  te.exe --install-update install the latest signed release without a window
  te.exe --version        print the version

Home:       https://github.com/boode-hub/te
Support:    https://buymeacoffee.com/boode

Licences for the built-in fonts and the logo are in licenses\.
