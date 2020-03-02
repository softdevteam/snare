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

* String escapes (e.g. "\"") are now properly processed (previously they were
  ignored).


# snare 0.1.0 (2020-02-13)

First release.
