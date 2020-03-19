use cfgrammar::yacc::YaccKind;
use lrlex::LexerBuilder;
use lrpar::CTParserBuilder;
use rerun_except::rerun_except;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    rerun_except(&["snare.1", "snare.conf.5", "snare.conf.example"])?;
    let lex_rule_ids_map = CTParserBuilder::<u8>::new_with_storaget()
        .yacckind(YaccKind::Grmtools)
        .process_file_in_src("config.y")?;
    LexerBuilder::new()
        .rule_ids_map(lex_rule_ids_map)
        .process_file_in_src("config.l")?;

    Ok(())
}
