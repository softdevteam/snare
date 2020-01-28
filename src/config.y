%start Options
%avoid_insert "INT" "STRING"

%%

Options -> Result<Vec<GenericOption<StorageT>>, ()>:
    Options Option { flattenr($1, $2) }
  | { Ok(vec![]) }
  ;

Option -> Result<GenericOption<StorageT>, ()>:
    "EMAIL" "=" "STRING" { Ok(GenericOption::Email(map_err($3)?)) }
  | "MAXJOBS" "=" "INT" { Ok(GenericOption::MaxJobs(map_err($3)?)) }
  | "PORT" "=" "INT" { Ok(GenericOption::Port(map_err($3)?)) }
  | "REPOSDIR" "=" "STRING" { Ok(GenericOption::ReposDir(map_err($3)?)) }
  | "SECRET" "=" "STRING" { Ok(GenericOption::Secret(map_err($3)?)) }
  ;

%%
use lrpar::Lexeme;

use crate::config::GenericOption;

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
