use proc_macro::TokenStream;
use proc_macro2::{Ident, Span};
use quote::quote;
use syn::{parse_macro_input, LitInt};

#[proc_macro]
pub fn generate_smith_waterman(input: TokenStream) -> TokenStream {
    let width = parse_macro_input!(input as LitInt)
        .base10_parse::<usize>()
        .unwrap();
    let width_with_padding = width + 1;
    let score_type = if width > 24 { quote!(u16) } else { quote!(u8) };

    let function_name = Ident::new(
        &format!("smith_waterman_inter_simd_{}", width),
        Span::call_site(),
    );

    let expanded = quote! {
        pub fn #function_name(needle: &str, haystacks: &[&str]) -> [u16; SIMD_WIDTH] {
            let needle = needle.as_bytes().iter().map(|x| *x as #score_type).collect::<Vec<#score_type>>();
            let needle_len = needle.len();
            let haystack_len = haystacks.iter().map(|x| x.len()).max().unwrap();

            // Convert haystacks to a static array of bytes chunked for SIMD
            let mut haystack = [[0; SIMD_WIDTH]; #width];
            for (char_idx, haystack_slice) in haystack.iter_mut().enumerate() {
                for str_idx in 0..SIMD_WIDTH {
                    if let Some(char) = haystacks[str_idx].as_bytes().get(char_idx) {
                        haystack_slice[str_idx] = *char as #score_type;
                    }
                }
            }

            // State
            let mut prev_col_score_simds: [Simd<#score_type, SIMD_WIDTH>; #width_with_padding] = [Simd::splat(0); #width_with_padding];
            let mut left_gap_penalty_masks = [Mask::splat(true); #width];
            let mut all_time_max_score = Simd::splat(0);

            // Delimiters
            let mut is_delimiter_masks = [Mask::splat(false); #width];
            let space_delimiter = Simd::splat(" ".bytes().next().unwrap() as #score_type);
            let slash_delimiter = Simd::splat("/".bytes().next().unwrap() as #score_type);
            let dot_delimiter = Simd::splat(".".bytes().next().unwrap() as #score_type);
            let comma_delimiter = Simd::splat(",".bytes().next().unwrap() as #score_type);
            let underscore_delimiter = Simd::splat("_".bytes().next().unwrap() as #score_type);
            let dash_delimiter = Simd::splat("-".bytes().next().unwrap() as #score_type);
            let delimiter_bonus = Simd::splat(DELIMITER_BONUS as #score_type);

            // Capitalization
            let capital_start = Simd::splat("A".bytes().next().unwrap() as #score_type);
            let capital_end = Simd::splat("Z".bytes().next().unwrap() as #score_type);
            let capitalization_bonus = Simd::splat(CAPITALIZATION_BONUS as #score_type);
            let to_lowercase_mask = Simd::splat(0x20);

            // Scoring params
            let gap_open_penalty = Simd::splat(GAP_OPEN_PENALTY as #score_type);
            let gap_extend_penalty = Simd::splat(GAP_EXTEND_PENALTY as #score_type);

            let match_score = Simd::splat(MATCH_SCORE as #score_type);
            let mismatch_score = Simd::splat(MISMATCH_PENALTY as #score_type);
            let prefix_match_score = Simd::splat((MATCH_SCORE + PREFIX_BONUS) as #score_type);
            let first_char_match_score = Simd::splat((MATCH_SCORE * FIRST_CHAR_MULTIPLIER) as #score_type);
            let first_char_prefix_match_score =
                Simd::splat(((MATCH_SCORE + PREFIX_BONUS) * FIRST_CHAR_MULTIPLIER) as #score_type);

            let zero: Simd<#score_type, SIMD_WIDTH> = Simd::splat(0);

            for i in 1..=needle_len {
                let match_score = if i == 1 {
                    first_char_match_score
                } else {
                    match_score
                };
                let prefix_match_score = if i == 1 {
                    first_char_prefix_match_score
                } else {
                    prefix_match_score
                };

                let needle_char = Simd::splat(needle[i - 1] as #score_type);
                let mut up_score_simd = Simd::splat(0);
                let mut up_gap_penalty_mask = Mask::splat(true);
                let mut curr_col_score_simds: [Simd<#score_type, SIMD_WIDTH>; #width_with_padding] = [Simd::splat(0); #width_with_padding];

                for j in 1..=haystack_len {
                    // Load chunk and remove casing
                    let cased_haystack_simd = Simd::<#score_type, SIMD_WIDTH>::from_slice(&haystack[j - 1]);
                    let capital_mask = cased_haystack_simd
                        .simd_ge(capital_start)
                        .bitand(cased_haystack_simd.simd_le(capital_end));
                    let haystack_simd = cased_haystack_simd | capital_mask.select(to_lowercase_mask, zero);

                    // Give a bonus for prefix matches
                    let match_score = Mask::splat(j == 1).select(prefix_match_score, match_score);

                    // Calculate diagonal (match/mismatch) scores
                    let diag = prev_col_score_simds[j - 1];
                    let match_mask = needle_char.simd_eq(haystack_simd);
                    let diag_score = match_mask.select(
                        diag + match_score
                            + is_delimiter_masks[j - 1].select(delimiter_bonus, zero)
                            + capital_mask.bitand(Mask::splat(j != 1)).select(capitalization_bonus, zero),
                        diag.simd_gt(mismatch_score)
                            .select(diag - mismatch_score, zero),
                    );

                    // Load and calculate up scores
                    let up_gap_penalty = up_gap_penalty_mask.select(gap_open_penalty, gap_extend_penalty);
                    let up_score = up_score_simd
                        .simd_gt(up_gap_penalty)
                        .select(up_score_simd - up_gap_penalty, zero);

                    // Load and calculate left scores
                    let left = prev_col_score_simds[j];
                    let left_gap_penalty_mask = left_gap_penalty_masks[j - 1];
                    let left_gap_penalty =
                        left_gap_penalty_mask.select(gap_open_penalty, gap_extend_penalty);
                    let left_score = left
                        .simd_gt(left_gap_penalty)
                        .select(left - left_gap_penalty, zero);

                    // Calculate maximum scores
                    let max_score = diag_score
                        .simd_max(up_score)
                        .simd_max(left_score)
                        .simd_max(zero);

                    // Update gap penalty mask
                    let diag_mask = max_score.simd_eq(diag_score);
                    up_gap_penalty_mask = max_score.simd_ne(up_score).bitor(diag_mask);
                    left_gap_penalty_masks[j - 1] = max_score.simd_ne(left_score).bitor(diag_mask);

                    // Update delimiter mask
                    is_delimiter_masks[j - 1] = space_delimiter
                        .simd_eq(needle_char)
                        .bitor(slash_delimiter.simd_eq(needle_char))
                        .bitor(dot_delimiter.simd_eq(needle_char))
                        .bitor(comma_delimiter.simd_eq(needle_char))
                        .bitor(underscore_delimiter.simd_eq(needle_char))
                        .bitor(dash_delimiter.simd_eq(needle_char));

                    // Store the scores for the next iterations
                    up_score_simd = max_score;
                    curr_col_score_simds[j] = max_score;

                    // Store the maximum score across all runs
                    all_time_max_score = all_time_max_score.simd_max(max_score);
                }

                prev_col_score_simds = curr_col_score_simds;
            }

            let mut max_scores_vec = [0; SIMD_WIDTH];
            for i in 0..SIMD_WIDTH {
                max_scores_vec[i] = all_time_max_score[i] as u16;
            }
            max_scores_vec
        }
    };

    TokenStream::from(expanded)
}
