%start TopLevelOptions
%avoid_insert "INT" "STRING"

%%

TopLevelOptions -> Result<Vec<TopLevelOption>, ()>:
    TopLevelOptions TopLevelOption { flattenr($1, $2) }
  | { Ok(vec![]) }
  ;

TopLevelOption -> Result<TopLevelOption, ()>:
    "GITHUB" "{" OptionsOrMatches "}" {
        let (options, matches) = $3?;
        Ok(TopLevelOption::GitHub($1.unwrap_or_else(|x| x).span(), options, matches))
    }
  | "LISTEN" "=" "STRING" ";" { Ok(TopLevelOption::Listen(map_err($3)?)) }
  | "MAXJOBS" "=" "INT" ";" { Ok(TopLevelOption::MaxJobs(map_err($3)?)) }
  | "USER" "=" "STRING" ";" { Ok(TopLevelOption::User(map_err($3)?)) }
  ;

OptionsOrMatches -> Result<(Vec<ProviderOption>, Vec<Match>), ()>:
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

ProviderOption -> Result<ProviderOption, ()>:
    "REPOSDIR" "=" "STRING" ";" { Ok(ProviderOption::ReposDir(map_err($3)?)) }
  ;

Matches -> Result<Vec<Match>, ()>:
    Matches Match { flattenr($1, $2) }
  | { Ok(vec![]) }
  ;

Match -> Result<Match, ()>:
    "MATCH" "STRING" "{" PerRepoOptions "}" { Ok(Match{re: map_err($2)?, options: $4? }) }
  ;

PerRepoOptions -> Result<Vec<PerRepoOption>, ()>:
    PerRepoOptions PerRepoOption { flattenr($1, $2) }
  | { Ok(vec![]) }
  ;

PerRepoOption -> Result<PerRepoOption, ()>:
    "EMAIL" "=" "STRING" ";" { Ok(PerRepoOption::Email(map_err($3)?)) }
  | "QUEUE" "=" QueueKind ";" {
        let (span, qkind) = $3?;
        Ok(PerRepoOption::Queue(span, qkind))
    }
  | "SECRET" "=" "STRING" ";" { Ok(PerRepoOption::Secret(map_err($3)?)) }
  | "TIMEOUT" "=" "INT" ";" { Ok(PerRepoOption::Timeout(map_err($3)?)) }
  ;

QueueKind -> Result<(Span, QueueKind), ()>:
    "EVICT" { Ok((map_err($1)?, QueueKind::Evict)) }
  | "PARALLEL" { Ok((map_err($1)?, QueueKind::Parallel)) }
  | "SEQUENTIAL" { Ok((map_err($1)?, QueueKind::Sequential)) }
  ;

%%
use lrpar::{Lexeme, Span};

use crate::config_ast::{TopLevelOption, Match, PerRepoOption, ProviderOption, QueueKind};

fn map_err<StorageT: Copy>(r: Result<Lexeme<StorageT>, Lexeme<StorageT>>)
    -> Result<Span, ()>
{
    r.map(|x| x.span()).map_err(|_| ())
}

/// Flatten `rhs` into `lhs`.
fn flattenr<T>(lhs: Result<Vec<T>, ()>, rhs: Result<T, ()>) -> Result<Vec<T>, ()> {
    let mut flt = lhs?;
    flt.push(rhs?);
    Ok(flt)
}
