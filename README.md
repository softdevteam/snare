# snare

`snare` is a GitHub webhooks daemon. When `snare` receives a webhook event from
a given repository, it authenticates the request, and then executes a
user-defined "per-repo program" with information about the webhook event.


## Install

`snare` requires rustc-1.40.0 or greater.

To install `snare` on a per-user basis, use `cargo install snare`.

To install `snare` globally and/or for packaging purposes, download the latest
stable version from [`snare`'s homepage](https://tratt.net/laurie/src/snare/).
You may use `cargo` to build snare locally or you may use the `Makefile` to
build and install `snare` in traditional Unix fashion. `make install` defaults
to installing in `/usr/local`: you can override this by setting the `PREFIX`
variable to another path (e.g. `PREFIX=/opt/local make`).


## Quick setup

`snare` has the following command-line format:

```
Usage: snare [-c <config-path>] [-d]
```

where:

 * `-c <config-path>` is a path to a `snare.conf` configuration file. If not
   specified, `snare` will assume the configuration file is located at
   `/etc/snare/snare.conf`.
 * `-d` tells `snare` *not* to daemonise: in other words, `snare` stays in the
   foreground. This can be useful for debugging.

The [man page for snare](https://softdevteam.github.io/snare/snare.1.html) contains
more details.

The minimal recommended configuration file is:

```
listen = "<ip-address>:<port>";

github {
  match ".*" {
    cmd = "/path/to/prps/%o/%r %e %j";
    email = "<email-address>";
    secret = "<secret>";
  }
}
```

where:

 * `ip-address` is either an IPv4 or IPv6 address and `port` a port on which an
   HTTP server will listen.
 * `cmd` is the command that will be executed when a webhook is received. In
   this case, `/path/to/prps` is a path to a directory where per-repo programs
   are stored. For a repository `repo` owned by `owner` the command:

     ```
     /path/to/prps/<owner>/<repo> <event> <path-to-github-json>
     ```

   will be run. The file `<repo>` must be executable. Note that commands are
   run with their current working directory set to a temporary directory to
   which they can freely write and which will be automatically removed when
   they have completed.
 * `email-address` is an email address to which any errors running per-repo
   programs will be sent (warning: full stderr/stdout will be sent, so consider
   carefully whether these have sensitive information or not). This uses
   the `sendmail` command to send email: you should ensure that you have
   installed, set-up, and enabled a suitable `sendmail` clone.
 * `secret` is the GitHub secret used to sign the webhook request and thus
   allowing `snare` to tell the difference between genuine webhook requests
   and those from malfeasants.

The [man page for
snare.conf](https://softdevteam.github.io/snare/snare.conf.5.html) contains the
complete list of configuration options.


## Commands

`snare` can be used to run any command runnable from the Unix shell. The
"per-repo program" model as documented above is one common way of doing this.
For example, `snare`'s GitHub
repository is
[`https://github.com/softdevteam/snare`](https://github.com/softdevteam/snare).
If we set up a web hook up for that repository that notifies us of pull request
events, then with the above `snare.conf`, the command:

```sh
<repo-programs-dir>/softdevteam/snare pull_request /path/to/json
```

will be executed, where: `pull_request` is the name of the GitHub event; and
`/path/to/json` is a path to a file containing the complete GitHub JSON for
that event. The `softdevteam/snare` program can then execute whatever it wants.
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


## Integration with GitHub

`snare` runs an HTTP server which GitHub can send webhook requests to.
Configuring a webhook for a given GitHub repository is relatively simple: go to
that repository, then `Settings > Webhooks > Add webhook`. For `payload`,
specify `http://yourmachine.com:port/`, specify a `secret` (which you will then
reuse as the `secret` in `snare.conf`) and then choose which events you wish
GitHub to deliver. For example, the default `Just the push event` works well
with the email diff sending per-repo program above, but you can specify
whichever events you wish.


## HTTPS/TLS

`snare` runs an HTTP server. If you wish, as is recommended, to send your
webhooks over an encrypted connection, you will need to run a proxy in front of
snare e.g.
[nginx](https://docs.nginx.com/nginx/admin-guide/web-server/reverse-proxy/) or
[relayd](https://man.openbsd.org/relayd.8).
