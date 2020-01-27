%start TopLevelOptions
%avoid_insert "INT" "STRING"

%%

TopLevelOptions -> Result<Vec<TopLevelOption<StorageT>>, ()>:
    TopLevelOptions TopLevelOption { flattenr($1, $2) }
  | { Ok(vec![]) }
  ;

TopLevelOption -> Result<TopLevelOption<StorageT>, ()>:
    "EMAIL" "=" "STRING" { Ok(TopLevelOption::Email(map_err($3)?)) }
  | "MAXJOBS" "=" "INT" { Ok(TopLevelOption::MaxJobs(map_err($3)?)) }
  | "PORT" "=" "INT" { Ok(TopLevelOption::Port(map_err($3)?)) }
  | "REPOSDIR" "=" "STRING" { Ok(TopLevelOption::ReposDir(map_err($3)?)) }
  | "SECRET" "=" "STRING" { Ok(TopLevelOption::Secret(map_err($3)?)) }
  ;

%%
use lrpar::Lexeme;

use crate::config::TopLevelOption;

type StorageT = u8;

fn map_err<StorageT>(r: Result<Lexeme<StorageT>, Lexeme<StorageT>>)
    -> Result<Lexeme<StorageT>, ()>
{
    r.map_err(|_| ())
}

/// Flatten `rhs` into `lhs`.
fn flattenr<T>(lhs: Result<Vec<T>, ()>, rhs: Result<T, ()>) -> Result<Vec<T>, ()> {
    let mut flt = lhs?;
    flt.push(rhs?);
    Ok(flt)
}
