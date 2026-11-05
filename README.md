# class

Google Classroom from the terminal.

## Install

```sh
cargo install --path .
```

Requires a Chromium-based browser (Chrome, Chromium, Helium, Brave or Edge).

## Log in

```sh
class auth login                 # log in in a browser window
class auth login --from helium   # reuse an existing browser session
```

## Use

```sh
class list            # pending assignments
class get 3           # download assignment #3 into a new folder
cd <folder>
class submit -n       # preview what will be sent
class submit          # upload and turn in
```

Already have a folder? Link it first:

```sh
class link 3
```

`class submit` sends a single file as is, or zips multiple files. It skips files created by
`class get`, anything in `.gitignore` or `.classignore`, and common junk like `node_modules`.

Run `class --help` for all commands.
