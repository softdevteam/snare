# snare

`snare` is a simple GitHub webhooks runner. When a request comes in, it runs an
arbitrary program for that repository informing it of the event -- that program
can then perform whatever actions it wants.


## Basic usage

`snare` has the following command-line format:

```
Usage: snare [-c <config-path>]
```

where:

 * `<config-path>` is a path to a `snare.conf` configuration file. If not
   specified explicitly, the following locations will be searched, in order:
     * `/etc/snare.conf/`
     * `~/.snare.conf`


## Configuration file

The configuration file supports the following options:

 * `email = "<address>"` optionally specifies an email address to which any
   errors running per-repo programs will be sent (warning: full stderr/stdout
   will be sent, so consider carefully whether these have sensitive information
   or not).
 * `maxjobs = <int>` is an (optional) non-zero integer specifying the maximum
   number of jobs to run in parallel. Defaults to the number of CPUs in the
   machine.
 * `port = <int>` is a mandatory port number to listen on (e.g. 4567).
 * `reposdir = "<path>"` is the directory where the per-repo programs are
   stored. For a repository `repo` owned by `user` the command
   `<reposdir>/<user>/<repo> <event> <path to GitHub JSON>` will be run. Note
   that per-repo programs are run with their current working directory set to a
   temporary directory to which they can freely write and which will be
   automatically removed when they have completed.
 * `secret = "<secret>"` is the mandatory GitHub secret which guarantees that
   hooks are coming from your GitHub repository and not a malfeasant.

The minimal configuration file is thus:

```
port = <port>
reposdir = "<path>"
secret = "<secret>"
```


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


## Example repository program

If we want to only execute commands when a pull request is merged, your
per-repo program might start as follows:

```sh
#! /bin/sh

# Ignore everything except pull request events
if [ $1 != "pull_request" ]; then
    exit 0
fi

# Ignore pull request events that aren't closing a pull request
if [ "X`jq .action $2 | tr -d '\"'`" != "Xclosed" ]; then
    exit 0
fi

# Ignore close events unless they merged changes in
if [ "X`jq .pull_request.merged $2 | tr -d '\"'`" != "Xtrue" ]; then
    exit 0
fi
```

where [`jq`](https://stedolan.github.io/jq/) is a command-line JSON processor.
If all three of those `if` statements succeed, then we know that a pull request
has been merged. As this suggests, some GitHub events are slightly trickier
than others to process and writing the above in shell script doesn't make it
particularly easy to see the core logic. However, users can equally well write
such programs in other languages if they prefer (i.e. you don't need to write
shell scripts for this if you don't want to).
