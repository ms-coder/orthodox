# OFM v1.0 spec coverage (T9.5)

Date: 2026-11-27. Branch `main`. Method: static reading of code, keymaps and test names. Nothing was built or run, so "tested by" means the test exists, not that it passed; visual and timing criteria (smooth scroll, 10 GB open, high contrast) were not measured.

All 130 command ids in `ofm-core/src/commands.rs` have a registered handler. Paths are relative to `crates/`.

| ID | Status | Implemented in | Tested by | Gap |
|---|---|---|---|---|
| F01 | done | ofm-app/src/window/layout.rs | `session_survives_a_restart`, `ratio_conversions` | none found |
| F02 | done | ofm-app/src/widgets/file_list/ | `parse_modes_and_columns`, `brief_scrolls_whole_columns` | Custom columns set only through config; smoothness not measured |
| F03 | done | ofm-core/src/panel/sort.rs | `natural_order_in_view`, `sort_names_round_trip` | none found |
| F04 | done | ofm-app/src/chrome/statusline.rs | `summaries`, `cursor_text` | Free-space display has no test |
| F05 | done | ofm-app/src/navigation/tabs.rs | `keys_tabs_and_view_mode`, `keys_tab_and_cursor_then_quit_saves_session` | none found |
| F06 | partial | ofm-app/src/navigation/quicksearch.rs | `typing_modes`, `quick_search_prefix_then_substring_with_wrap` | Far preset: Alt+M opens select-by-mask, so quick search cannot be started with "m" via Alt (Ctrl+S works) |
| F07 | done | ofm-app/src/dialogs/hotlist.rs, navigation/history.rs | `back_forward_and_truncate`, `round_trip_with_non_utf8_bookmark` | none found |
| F08 | done | ofm-app/src/navigation/mount.rs | none | No tests; a failed mount is only logged (user sees nothing); "Trash" entry is the home trash only |
| F09 | done | ofm-app/src/navigation/goto.rs | `keys_enter_dir_and_go_up`, `t23_commands_have_handlers_and_keys_in_far_and_tc` | none found |
| F10 | partial | ofm-app/src/navigation/selection.rs, dialogs/mask.rs | `selection_semantics`, `mask_lists_respect_braces_and_exclusions` | Space does not toggle selection in the Far preset (it types into the command line); no Shift+Left/Right |
| F11 | partial | ofm-app/src/cmdline/ | `typed_text_appears_and_plain_cd_navigates`, `history_stepping_restores_the_draft` | Ctrl+X is clipboard cut, not history-next (Ctrl+Shift+E instead; deliberate, docs/COMMANDS.md, SPEC not updated) |
| F12 | done | ofm-app/src/terminal/ | `shell_cd_moves_panel`, `panel_navigation_syncs_shell_cwd` | cwd sync only for bash/zsh/fish |
| F13 | done | ofm-app/src/dialogs/copy.rs, ofm-jobs/src/copy.rs | `shift_f6_renames_in_place`, `copy_applies_rename_masks` | none found |
| F14 | done | ofm-jobs/src/ops/trash.rs | `f8_moves_to_trash_with_trashinfo`, `other_mount_uses_topdir_trash_with_relative_path` | Restore from GNOME Files is a manual check |
| F15 | done | ofm-jobs/src/engine.rs, ofm-app/src/jobs/view.rs | `queued_jobs_run_in_order_parallel_jobs_run_at_once`, `jobs_view_toggles_and_quit_with_running_job_prompts` | none found |
| F16 | done | ofm-app/src/dialogs/conflict.rs | `f5_conflict_answers_skip_overwrite_and_if_newer`, `ctrl_a_in_conflict_dialog_applies_to_all` | none found |
| F17 | done | ofm-jobs/src/copy.rs, fs/real.rs | `copy_roundtrip_preserves_content_mode_mtime_and_created_list`, `sparse_files_stay_sparse` | none found (ownership-as-root path not checked for a test) |
| F18 | done | ofm-app/src/dialogs/link.rs | `absolute_relative_and_hard` | none found |
| F19 | done | ofm-app/src/dialogs/properties.rs | `only_changed_bits_are_sent`, `f3_on_a_directory_calculates_its_size` | none found |
| F20 | done | ofm-app/src/batchrename/ | `batch_rename_flow_swap_collision_and_undo`, `ctrl_shift_m_swaps_names_with_a_gui_editor_and_undo_restores_them` | Undo is keyboard only (Ctrl+Alt+Z), absent from the menus |
| F21 | done | ofm-viewer/, ofm-app/src/viewer/ | `f3_opens_f4_hex_f7_search_and_esc_closes`, `sparse_ten_gigabyte_file_opens_instantly` | none found |
| F22 | done | ofm-app/src/quickview/ | `quick_view_follows_the_cursor_and_restores_the_panel`, `burst_is_debounced_into_few_events` | none found |
| F23 | done | ofm-app/src/desktop/editor.rs | `visual_before_editor_before_gnome`, `terminal_editor_runs_in_the_embedded_terminal` | none found |
| F24 | done | ofm-search/, ofm-app/src/dialogs/find.rs | `find_scenarios`, `follow_symlinks_descends_and_dedups` | none found |
| F25 | done | ofm-core/src/panelize.rs, ofm-app/src/search/ | `f10_panelizes_results_and_f5_copies_a_real_file`, `panelize_command_shows_output_and_file_ops_work_on_real_files` | none found |
| F26 | done | ofm-vfs/src/archive/ | `zip_navigate_extract_refuse_and_view`, `encrypted_zip_password_dialog_retry_and_remembered` | none found |
| F27 | done | ofm-app/src/compare/, ofm-core/src/compare.rs | `shift_f2_enter_marks_the_differences_in_both_panels`, `content_mode_marks_files_that_differ_in_bytes` | none found |
| F28 | done | ofm-core/src/menu_expand.rs, ofm-app/src/usermenu/ | `user_menu_keys_run_entries`, `local_menu_needs_trust` | none found |
| F29 | done | ofm-config/src/keymap.rs | `every_preset_covers_every_command_without_conflicts`, `detects_conflicts` | none found |
| F30 | done | ofm-app/src/theme/, ofm-config/src/theme.rs | `chrome_themes_and_session`, `first_match_wins_and_attrs` | See Themes row |
| F31 | done | ofm-app/src/desktop/ | `clipboard_keys_copy_and_move_between_panels`, `dragging_a_file_to_the_other_panel_copies_and_shift_moves` | Exchange with GNOME Files itself is a manual check |
| F32 | done | ofm-app/src/chrome/, widgets/file_list/imp.rs | `menu_bar_switches_closes_and_runs_with_real_keys`, `context_menu_opens_runs_and_closes_with_real_keys` | Batch-rename undo not reachable by mouse (see F20) |
| F33 | done | ofm-app/src/window/session.rs, navigation/session.rs | `session_survives_a_restart`, `session_state_survives_snapshot_and_restore` | none found |
| Themes | partial | ofm-config/defaults/themes/, ofm-app/src/theme/ | `shipped_themes_icons_follow_the_spec`, `adwaita_theme_uses_system_colours_and_keeps_animations` | `retro.monospace_grid` is parsed but never read by the app; no high-contrast variant for the fixed themes (the notify only re-reads system colours; not verified visually) |

Totals for F01-F33: 30 done, 3 partial, 0 missing. Themes section: partial.

## Gaps worth fixing before 1.0 (by user impact)

1. F10: Space does not mark files in the Far preset, although SPEC lists "Ins / Space" (many laptops have no Ins). Either toggle when the command line is empty, or amend SPEC.
2. F08: a failed mount gives no message (`mount_and_open` only logs). Show the error dialog.
3. F11: Ctrl+X history-next is not bound (it is Cut). Record the deviation in SPEC; no code change needed.
4. F06: Alt+M cannot start a quick search. Move select-by-mask off Alt+M, or document it.
5. Themes: fixed themes ignore GNOME high contrast (verify by eye first).
6. F20/F32: add "Undo batch rename" to the menu bar.
7. Themes: implement `retro.monospace_grid` or drop it from the format.
8. F08: add tests for building the drive-menu item list; F04: test the free-space label.
