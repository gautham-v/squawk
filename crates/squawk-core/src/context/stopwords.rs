//! Words the jargon pass never rewrites or joins into, whatever a repo's docs
//! say. Common English plus the everyday words of UIs and dev work: a README
//! that writes "open Settings" must not turn every "settings" you say into
//! "Settings". Lowercase; matched case-insensitively.

use std::collections::HashSet;
use std::sync::OnceLock;

const WORDS: &str = "
a able about above accept access account across act action actions active actually add added
address after again against age ago agent agents ahead air all allow almost alone along already
also always am among amount an and another answer any anyone anything anyway app apps apple
area around art as ask at audio author auto available away back bad bar base basic basically be
because become been before begin behind being below best better between big bit black block
blue board body book both bottom box break bring brown bug bugs build built bus business busy
but button buy by call called calendar came can cannot car card care case cat cause center
certain chair change changes chat check child choose city class clean clear click client close
code cold color come coming command common company complete computer config content context
control copy core could count country course cover create current cut dark data date day days
dead deal dear debug default delete design desk detail details dev did different dir do docs
does dog doing done door down draft draw drive drop during each early easy edge edit editor
either else email empty end engine enough enter entry error even event ever every example
except eye face fact fail fair fall false far fast feature feel few field file files fill final
find fine fire first fix flag flow folder follow food for form format forward found free friend
from front full fun function game gave get give go goes going gone good got great green group
grow guide had half hand handle happen happy hard has have he head hear heard help her here
hey high him his history hit hold home hook hope host hot hour hours house how however i idea
if image import in index info input inside instead into is issue issues it item items its
itself just keep key keyboard kind know label land large last late later layout lead learn
least leave left less let letter level library life light like line lines link list little
live load local lock log long look lot love low made main make man many map mark match may maybe
me mean meet meeting meetings memory menu message method might mind minute mode model money
month more morning most move much music must my name near need never new news next nice night
no none normal not note notes nothing now number of off offer office often oh ok okay old on
once one only open option options or order other our out output over own page pane panel paper
part pass past path people per person phone pick piece place plan play please plus point popup
port post power press pretty print private problem process project public pull push put
question quick quite rather read ready real really reason record red release remove repo report
request reset rest result return review right road room root row rule run safe said same save
saw say school screen script search second section see seem seen select self send server service
session set settings setup shape share she short should show side sign simple since single site
size skill skills small so some something sort sound source space speak special start state
status step still stop store story street strong style such sure switch system tab table tag
take talk task team tell term terms test tests text than thank thanks that the their them then
there these they thing things think this those though thought three through time title to today
together told too took tool tools top total touch track tree true try turn two type under until
up update upon us use used user users using value version very view wait walk want was watch
water way we week well went were what when where whether which while white who whole why will
window with within without word words work working world would write wrong year yes yet you
young your yourself zero
claude codex license readme changelog todo
";

pub(crate) fn is_stopword(word: &str) -> bool {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    let set = SET.get_or_init(|| WORDS.split_whitespace().collect());
    set.contains(word.to_lowercase().as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_insensitive() {
        assert!(is_stopword("Settings"));
        assert!(is_stopword("the"));
        assert!(!is_stopword("Tidewell"));
        assert!(!is_stopword("tokio"));
    }
}
