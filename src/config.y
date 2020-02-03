%start TopLevelOptions
%avoid_insert "INT" "STRING"

%%

TopLevelOptions -> Result<Vec<TopLevelOption<StorageT>>, ()>:
    TopLevelOptions TopLevelOption { flattenr($1, $2) }
  | { Ok(vec![]) }
  ;

TopLevelOption -> Result<TopLevelOption<StorageT>, ()>:
    "GITHUB" "{" OptionsOrMatches "}" {
        let (options, matches) = $3?;
        Ok(TopLevelOption::GitHub($1.unwrap_or_else(|x| x), options, matches))
    }
  | "MAXJOBS" "=" "INT" { Ok(TopLevelOption::MaxJobs(map_err($3)?)) }
  | "PORT" "=" "INT" { Ok(TopLevelOption::Port(map_err($3)?)) }
  ;

OptionsOrMatches -> Result<(Vec<ProviderOption<StorageT>>, Vec<Match<StorageT>>), ()>:
    OptionsOrMatches ProviderOption {
        let (mut options, matches) = $1?;
        options.push($2?);
        Ok((options, matches))
    }
  | OptionsOrMatches Match {
        let (options, mut matches) = $1?;
        matches.push($2?);
        Ok((options, matches))
    }
  | { Ok((vec![], vec![])) }
  ;

ProviderOption -> Result<ProviderOption<StorageT>, ()>:
    "REPOSDIR" "=" "STRING" { Ok(ProviderOption::ReposDir(map_err($3)?)) }
  ;

Matches -> Result<Vec<Match<StorageT>>, ()>:
    Matches Match { flattenr($1, $2) }
  | { Ok(vec![]) }
  ;

Match -> Result<Match<StorageT>, ()>:
    "MATCH" "STRING" "{" PerRepoOptions "}" { Ok(Match{re: map_err($2)?, options: $4? }) }
  ;

PerRepoOptions -> Result<Vec<PerRepoOption<StorageT>>, ()>:
    PerRepoOptions PerRepoOption { flattenr($1, $2) }
  | { Ok(vec![]) }
  ;

PerRepoOption -> Result<PerRepoOption<StorageT>, ()>:
    "EMAIL" "=" "STRING" { Ok(PerRepoOption::Email(map_err($3)?)) }
  | "QUEUE" "=" QueueKind {
        let (lexeme, qkind) = $3?;
        Ok(PerRepoOption::Queue(lexeme, qkind))
    }
  | "SECRET" "=" "STRING" { Ok(PerRepoOption::Secret(map_err($3)?)) }
  | "TIMEOUT" "=" "INT" { Ok(PerRepoOption::Timeout(map_err($3)?)) }
  ;

QueueKind -> Result<(Lexeme<StorageT>, QueueKind), ()>:
    "EVICT" { Ok((map_err($1)?, QueueKind::Evict)) }
  | "PARALLEL" { Ok((map_err($1)?, QueueKind::Parallel)) }
  | "SEQUENTIAL" { Ok((map_err($1)?, QueueKind::Sequential)) }
  ;

%%
use lrpar::Lexeme;

use crate::config_ast::{TopLevelOption, Match, PerRepoOption, ProviderOption, QueueKind};

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
