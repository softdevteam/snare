# snare 0.3.0 (2020-03-06)

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
