# snare 0.4.9 (2024-01-08)

* Remove hyper/tokio in favour of a simple internal HTTP server. This
  reduces the number of library dependencies by about 25%.

* Improve logging: errors, warnings, and information are now differentiated.
  The `-v` switch increases the logging level. Defaults to "only report
  errors".

* Default to `/bin/sh` if `$SHELL` isn't set when running commands.

* Add a system test suite.

* Respect `DESTDIR`, and try to pick a more appropriate location for man pages,
  in installation.


# snare 0.4.8 (2023-03-08)

* Update dependencies.


# snare 0.4.7 (2023-02-06)

* Update dependencies, including moving from the unmaintained `json` crate to `serde_json`.


# snare 0.4.6 (2022-02-09)

* Update dependencies, including a security fix to the regex crate.

* Remove mention of `reposdir` from the documentation: it is deprecated and
  using it causes a warning.


# snare 0.4.5 (2022-02-09)

* Update dependencies.


# snare 0.4.4 (2021-10-26)

* Update dependencies.


# snare 0.4.3 (2021-06-11)

* Update many dependencies.


# snare 0.4.2 (2021-03-17)

* Update to tokio 1. Also update other dependencies, avoiding warnings over
  yanked (old) versions of pin-project-lite.


# snare 0.4.1 (2020-12-03)

* Documentation improvements, including more secure examples.

* Updated dependencies, solving a long-standing slow error leak.


# snare 0.4.0 (2020-05-13)

## Breaking changes

* The `email` option in `match` blocks has been replaced by the more generic
  `errorcmd`. To obtain the previous behaviour:

    ```
    email = "someone@example.com";
    ```

  should be changed to something like:

    ```
    errorcmd = "cat %s | mailx -s \"snare error: github.com/%o/%r\" someone@example.com";
    ```

  This assumes that the `mailx` command is installed on your machine.  As this
  example may suggest, `errorcmd` is much more flexible than `email`.  The
  syntax of `errorcmd` is the same as `cmd` with the addition that `%s` is
  expanded to the path of the failed job's combined stderr / stdout.

  `snare` informs users whose config contains `email` how to update to
  `errorcmd` to obtain the previous behaviour.

## Minor changes

* After daemonisation, all errors are now sent to syslog (previously a few
  errors could still be sent to stderr).

* Fix bug in parsing string escapes, where one character too many was
  consumed after `\"`.

* Use SIGCHLD to listen for child process exit, so that `snare` does not have
  to be woken up as often.



# snare 0.3.0 (2020-03-08)

## Breaking changes

* `snare` now only searches for a configuration file at
  `/etc/snare/snare.conf`; as before, you can specify an alternative location
  for `snare.conf` via the `-c` option.

* `snare` always changes its CWD to `/` (previously CWD was only altered if a
  `user` was specified).


## Minor changes

* When a command fails, the email sent now contains the owner and repository
  name in the subject.


# snare 0.2.0 (2020-03-02)

## Breaking changes

* The `github`-block level `reposdir` option has been removed. The more
  flexible `match`-block level `cmd` has been introduced. In essence:

    ```
    github {
      reposdir = "/path/to/prps";
      ...
    }
    ```

  should be changed to:

    ```
    github {
      match ".*" {
        cmd = "/path/to/reposdir/%o/%r %e %j";
      }
    }
    ```

  `snare` informs users whose config contains `repodir` how to update it.


## Minor changes

* `snare` now validates input derived from the webhook request so that it is
  safe to pass to the shell: GitHub owners, repositories, and events are all
  guaranteed to satisfy the regular expression `[a-zA-Z0-9._-]+` and not to be
  the strings `.` or `..`.

* String escapes (e.g. `"\""`) are now properly processed (previously they were
  ignored).


# snare 0.1.0 (2020-02-13)

First release.
