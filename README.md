# snare

`snare` is a simple GitHub webhooks runner. When a request comes in, it runs an
arbitrary program for that repository informing it of the event -- that program
can then perform whatever actions it wants.


## Basic usage

`snare` has the following command-line format:

```
Usage: snare [-c <config-path>] [-d]
```

where:

 * `-c <config-path>` is a path to a `snare.conf` configuration file. If not
   specified explicitly, the following locations will be searched, in order:
     * `~/.snare.conf`
     * `/etc/snare.conf/`
 * `-d` tells `snare` *not* to daemonise: in other words, `snare` stays in the
   foreground. This can be useful for debugging.


## Configuration file

The configuration file supports the following top-level options:

 * `listen = "<address>";` is a mandatory address and port number to listen on.
   The format of `address` is either:
     * IPv4: `x.x.x.x:port` e.g. `0.0.0.0:8765` will listen on port 8765 for
       all IPv4 addresses.
     * IPv6: `[x:x:x]:port` e.g. `[::]:8765` will listen on port 8764 for all
       IPv4 and IPv6 addresses
 * `maxjobs = <int>;` is an (optional) non-zero integer specifying the maximum
   number of jobs to run in parallel. Defaults to the number of CPUs in the
   machine.
 * `user = "<user-name>";` is an optional username that `snare` will try and
   change into after it has bound to a network port. Note that `snare` will
   refuse to run as `root` unless the `user` option is specified. As part of
   changing user, `snare`:
     * changes its uid, euid, suid to `user-name`'s GID.
     * changes its gid, egid, sgid to `user-name`'s primary GID.
     * change its CWD to `user-name`'s home directory.
     * sets the `$HOME` environment variable to `user-name`'s home directory.
     * sets the `$USER` environment variable to `user-name`
   Note that *all* other environment variables are passed through to child
   processes unchanged.
 * `github { ... }` specifies GitHub specific options.

The `github` block supports the following options:

 * `reposdir = "<path>";` is the directory where the per-repo programs are
   stored. For a repository `repo` owned by `user` the command
   `<reposdir>/<user>/<repo> <event> <path to GitHub JSON>` will be run. Note
   that per-repo programs are run with their current working directory set to a
   temporary directory to which they can freely write and which will be
   automatically removed when they have completed.

  * `match "<regex>" { <match-options> }` where `regex` is a regular expression
    in [Rust regex format](https://docs.rs/regex/) that must match against a
    `owner/repo` full repository name. If it matches, then `<match-options>`
    are set for that repository. Note that `regex` is implicitly embedded in
    `^<regex>$` i.e. `regex` must match against the full repository name and
    not a subset (so the regex `a/b` does not match against the full repository
    name `a/bc`, but the regex `a/b.*` does match against `a/bc`).

A `match` block supports the following options:

 * `email = "<address>";` optionally specifies an email address to which any
   errors running per-repo programs will be sent (warning: full stderr/stdout
   will be sent, so consider carefully whether these have sensitive information
   or not).
 * `queue = <evict|parallel|sequential>;` optionally specifies what to do when
   multiple requests for the same repository are queued at once:
     * `evict`: only run one job for this repository at a time. Additional jobs
       will stay on the queue: if a new job comes in for that repository, it
       evicts any previously queued jobs for that repository. In other words,
       for this repository there can be at most one running job and one queued
       job at any point.
     * `parallel`: run as many jobs for this repository in parallel as
       possible.
     * `sequential`: only run one job for this repository at a time. Additional
       jobs will stay on the queue and be executed in FIFO order.
 * `secret = "<secret>";` is an optional GitHub secret which guarantees that
   hooks are coming from your GitHub repository and not a malfeasant. Although
   this is optional, we *highly* recommend setting it in all cases. Note also
   that if a GitHub request is signed, but you have not specified a secret,
   then snare will return the request as "unauthorised" to remind you to use
   the secret at both ends.
 * `timeout = <timeout>;` optionally specifies the elapsed time in seconds that
   a process can run before being sent SIGTERM.

`match` blocks are evaluated in order from top to bottom with each successful
match overriding previous settings.  There is a default `match` block placed
before any user `match` blocks:

```
match ".*" {
  queue = sequential;
  timeout = 3600;
}
```

The minimal recommended configuration file is thus:

```
listen = "<address>:<port>";

github {
  reposdir = "<path>";
  match ".*" {
    email = "<email>";
  }
}
```

The top-to-bottom evaluation of `match` blocks allow users to specify defaults
which are only overridden for specific repositories. For example, for the
following configuration file:

```
listen = "<address>:<port>";

github {
  reposdir = "<path>";
  match ".*" {
    email = "abc@def.com";
    secret = "sec";
  }
  match "a/b" {
    email = "ghi@jkl.com";
  }
}
```

then the repositories will have the following settings:

  * `a/b`:
    * `queue = sequential`
    * `timeout = 3600`
    * `email = "ghi@jkl.com"`
    * `secret = "sec"`
  * `c/d`:
    * `queue = sequential`
    * `timeout = 3600`
    * `email = "abc@def.com"`
    * `secret = "sec"`


## Per-repo programs

When using snare, the *per-repo programs* do the actual work of executing
specific actions for a given repository.  For example, `snare`'s GitHub
repository is
[`https://github.com/softdevteam/snare`](https://github.com/softdevteam/snare).
If we set up a web hook up for that repository that notifies us of pull request
events, then the command:

```sh
<repo-programs-dir>/softdevteam/snare pull_request /path/to/json
```

will be executed, where: `pull_request` is the name of the GitHub event; and
`/path/to/json` is a path to a file containing the complete GitHub JSON for
that event. The `softdevteam_snare` program can then execute whatever it wants.
In order to work out precisely what event has happened, you will need to read
[GitHub's webhooks documentation](https://developer.github.com/webhooks/).


## Example per-repo program

Users can write per-repo programs in whatever system/language they wish, so
long as the matching file is marked as executable. The following simple example
uses shell script to send a list of commits and diffs to the address specified
in `$EMAIL` on each `push` event. It works for any public GitHub repository:

```sh
#! /bin/sh

set -euf

EMAIL="someone@something.com"

if [ "$1" != "push" ]; then
    exit 0
fi

repo_fullname=`jq .repository.full_name "$2" | tr -d '\"'`
repo_url=`jq .repository.html_url "$2" | tr -d '\"'`
before_hash=`jq .before "$2" | tr -d '\"'`
after_hash=`jq .after "$2" | tr -d '\"'`

git clone "$repo_url" repo
cd repo
git log --reverse -p "$before_hash..$after_hash" | mail -s "Push to $repo_fullname" "$EMAIL"
```

where [`jq`](https://stedolan.github.io/jq/) is a command-line JSON processor.
Depending on your needs, you can make this type of script arbitrarily more
complex and powerful (e.g. not cloning afresh on each pull).

Note that this program is deliberately untrusting of external input: it is
careful to quote all arguments obtained from JSON; and it uses a fixed
directory name (`repo`) rather than use a file name from JSON that might
include characters (e.g. `../..`) that would cause the script to leak data
about other parts of the file system.
