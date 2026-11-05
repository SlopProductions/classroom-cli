# class

Google Classroom from the terminal.

## Install

Homebrew (macOS, Linux):

```sh
brew tap slopproductions/classroom-cli https://github.com/SlopProductions/classroom-cli
brew install classroom-cli
```

Prebuilt binaries are attached to every [release](https://github.com/SlopProductions/classroom-cli/releases):

```sh
gh release download -R SlopProductions/classroom-cli -p "class-*-aarch64-apple-darwin.tar.gz"
```

From source:

```sh
cargo install --path .
```

Works on Linux and macOS. Requires a Chromium-based browser: Chrome, Helium, Brave, Arc,
Edge, Chromium, Vivaldi or Opera.

## Log in

```sh
class auth login                    # pick a browser: reuse its session or log in in a new window
class auth login --from helium      # reuse your Helium session
class auth login --browser /Applications/Helium.app
class auth browsers                 # show detected browsers
```

On macOS, click "Allow" if the keychain asks for access to your browser's Safe Storage.

## Use

```sh
class list            # pending assignments
class get 3           # download assignment #3 into a new folder
cd <folder>
class submit -n       # preview what will be sent
class submit          # upload and turn in
```

Clean up old assignments by marking them as done (no attachments):

```sh
class done 5                     # one assignment
class done --older-than 90 -n    # preview everything older than 90 days
class done --older-than 90       # mark them all
```

Already have a folder? Link it first:

```sh
class link 3
```

`class submit` sends a single file as is, or zips multiple files. It skips files created by
`class get`, anything in `.gitignore` or `.classignore`, and common junk like `node_modules`.

More:

```sh
class grades -s          # average score per course
class feed -u            # new announcements, materials and assignments
class calendar -o due.ics
class doctor             # check browser, session and connection
```

Show pending work in your zsh prompt (instant, refreshes in the background):

```sh
setopt PROMPT_SUBST
RPROMPT='$(class count)'
```

Run `class --help` for all commands.
