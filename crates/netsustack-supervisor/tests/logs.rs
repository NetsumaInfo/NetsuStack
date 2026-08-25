use netsustack_supervisor::logs::{AnsiTranscript, PlainLogStore};

mod logs {
    use super::*;
    use std::fs;

    #[test]
    fn decodes_fragmented_utf8_and_replaces_invalid_input() {
        let mut logs = PlainLogStore::memory(10);

        logs.ingest(&[b'c', b'a', b'f', 0xc3]).unwrap();
        logs.ingest(&[0xa9, b'\n', 0xff, b'!', b'\n']).unwrap();

        assert_eq!(logs.tail(10), ["café", "�!"]);
    }

    #[test]
    fn finish_emits_a_trailing_line_without_newline() {
        let mut logs = PlainLogStore::memory(10);
        logs.ingest(b"tail").unwrap();

        logs.finish_at("00:00:00").unwrap();

        assert_eq!(logs.tail(10), ["tail"]);
    }

    #[test]
    fn finish_settles_a_trailing_carriage_return() {
        let mut logs = PlainLogStore::memory(10);
        logs.ingest(b"progress\r").unwrap();

        logs.finish_at("00:00:00").unwrap();

        assert_eq!(logs.tail(10), ["progress"]);
    }

    #[test]
    fn finish_replaces_incomplete_utf8() {
        let mut logs = PlainLogStore::memory(10);
        logs.ingest(b"caf\xc3").unwrap();

        logs.finish_at("00:00:00").unwrap();

        assert_eq!(logs.tail(10), ["caf�"]);
    }

    #[test]
    fn finish_discards_an_incomplete_escape_sequence() {
        let mut logs = PlainLogStore::memory(10);
        logs.ingest(b"visible\x1b]hidden").unwrap();

        logs.finish_at("00:00:00").unwrap();

        assert_eq!(logs.tail(10), ["visible"]);
    }

    #[test]
    fn note_finalizes_partial_process_output_first() {
        let mut logs = PlainLogStore::memory(10);
        logs.ingest(b"partial").unwrap();

        logs.note_at("restart", "00:00:00").unwrap();

        assert_eq!(logs.tail(10), ["partial", "[netsustack] restart"]);
    }

    #[test]
    fn strips_fragmented_csi_and_osc_sequences() {
        let mut logs = PlainLogStore::memory(10);

        logs.ingest(b"plain \x1b[3").unwrap();
        logs.ingest(b"1mred\x1b[0m \x1b]0;hidden").unwrap();
        logs.ingest(b" title\x07visible\n").unwrap();
        logs.ingest(b"link \x1b]8;;https://example.test\x1b\\name\x1b]8;;\x1b\\\n")
            .unwrap();

        assert_eq!(logs.tail(10), ["plain red visible", "link name"]);
    }

    #[test]
    fn resolves_cr_crlf_bel_and_backspace() {
        let mut logs = PlainLogStore::memory(10);

        logs.ingest(b"step 1\rstep 2\r").unwrap();
        logs.ingest(b"done\r\nbeep\x07!\nabc\x08d\n").unwrap();

        assert_eq!(logs.tail(10), ["done", "beep!", "abd"]);
    }

    #[test]
    fn caps_unterminated_text_without_splitting_the_ring_bound() {
        let mut logs = PlainLogStore::memory(20);

        logs.ingest(&vec![b'a'; 20_000]).unwrap();
        logs.ingest(b"\n").unwrap();

        let lines = logs.tail(20);
        assert!(lines.iter().all(|line| line.len() <= 8_192));
        assert_eq!(lines.concat(), "a".repeat(20_000));
    }

    #[test]
    fn oversized_csi_stays_discarded_across_chunks_until_its_final_byte() {
        let mut logs = PlainLogStore::memory(10);

        logs.ingest(b"before\x1b[").unwrap();
        logs.ingest(&vec![b'1'; 5_000]).unwrap();
        logs.ingest(b"222mvisible\n").unwrap();

        assert_eq!(logs.tail(10), ["beforevisible"]);
    }

    #[test]
    fn oversized_osc_stays_discarded_across_chunks_until_bel_or_split_st() {
        let mut logs = PlainLogStore::memory(10);

        logs.ingest(b"bel:\x1b]").unwrap();
        logs.ingest(&vec![b'x'; 5_000]).unwrap();
        logs.ingest(b"hidden").unwrap();
        logs.ingest(b"\x07visible\n").unwrap();

        logs.ingest(b"st:\x1b]").unwrap();
        logs.ingest(&vec![b'y'; 5_000]).unwrap();
        logs.ingest(b"hidden\x1b").unwrap();
        logs.ingest(b"\\visible\n").unwrap();

        assert_eq!(logs.tail(10), ["bel:visible", "st:visible"]);
    }

    #[test]
    fn strips_all_seven_bit_control_strings_and_general_escape_sequences() {
        let mut logs = PlainLogStore::memory(10);

        logs.ingest(b"dcs:\x1bPpayload\x1b").unwrap();
        logs.ingest(b"\\visible\n").unwrap();
        logs.ingest(b"sos:\x1bXpayload\x1b\\visible\n").unwrap();
        logs.ingest(b"pm:\x1b^payload\x1b\\visible\n").unwrap();
        logs.ingest(b"apc:\x1b_payload\x1b\\visible\n").unwrap();
        logs.ingest(b"esc:\x1b(").unwrap();
        logs.ingest(b"Bvisible\x1b#8done\n").unwrap();

        assert_eq!(
            logs.tail(10),
            [
                "dcs:visible",
                "sos:visible",
                "pm:visible",
                "apc:visible",
                "esc:visibledone",
            ]
        );
    }

    #[test]
    fn strips_utf8_c1_control_sequences_and_fragmented_st() {
        let mut logs = PlainLogStore::memory(20);
        let c1 = concat!(
            "csi:\u{009b}31mvisible\n",
            "osc-bel:\u{009d}payload\u{0007}visible\n",
            "osc-st:\u{009d}payload\u{009c}visible\n",
            "dcs:\u{0090}payload\u{009c}visible\n",
            "sos:\u{0098}payload\u{009c}visible\n",
            "pm:\u{009e}payload\u{009c}visible\n",
            "apc:\u{009f}payload\u{009c}visible\n",
        );
        logs.ingest(c1.as_bytes()).unwrap();
        logs.ingest(b"split:").unwrap();
        logs.ingest(&[0xc2]).unwrap();
        logs.ingest(&[0x90]).unwrap();
        logs.ingest(b"payload").unwrap();
        logs.ingest(&[0xc2]).unwrap();
        logs.ingest(&[0x9c]).unwrap();
        logs.ingest(b"visible\n").unwrap();

        assert_eq!(
            logs.tail(20),
            [
                "csi:visible",
                "osc-bel:visible",
                "osc-st:visible",
                "dcs:visible",
                "sos:visible",
                "pm:visible",
                "apc:visible",
                "split:visible",
            ]
        );
    }

    #[test]
    fn can_and_sub_cancel_control_sequences_without_leaking_payload() {
        let mut logs = PlainLogStore::memory(10);

        logs.ingest(b"can:\x1bPpayload\x18visible\n").unwrap();
        logs.ingest(b"sub:\x1b]payload\x1avisible\n").unwrap();
        logs.ingest(b"csi:\x1b[123\x18visible\n").unwrap();

        assert_eq!(logs.tail(10), ["can:visible", "sub:visible", "csi:visible"]);
    }

    #[test]
    fn oversized_dcs_remains_discarded_until_split_st() {
        let mut logs = PlainLogStore::memory(10);

        logs.ingest(b"dcs:\x1bP").unwrap();
        logs.ingest(&vec![b'x'; 5_000]).unwrap();
        logs.ingest(b"still hidden\x1b").unwrap();
        logs.ingest(b"\\visible\n").unwrap();

        assert_eq!(logs.tail(10), ["dcs:visible"]);
    }

    #[test]
    fn partial_line_bound_never_splits_or_overruns_a_utf8_scalar() {
        let mut logs = PlainLogStore::memory(10);
        logs.ingest(&vec![b'a'; 8_191]).unwrap();
        logs.ingest("🦀\n".as_bytes()).unwrap();

        let lines = logs.tail(10);
        assert!(lines.iter().all(|line| line.len() <= 8_192));
        assert_eq!(lines.concat(), format!("{}🦀", "a".repeat(8_191)));
    }

    #[test]
    fn bounds_the_ring_tail_and_applies_limit_changes_immediately() {
        let mut logs = PlainLogStore::memory(3);
        logs.ingest(b"one\ntwo\nthree\nfour\n").unwrap();

        assert_eq!(logs.tail(2), ["three", "four"]);
        assert!(logs.tail(0).is_empty());

        logs.update_limits(2, 100).unwrap();
        assert_eq!(logs.tail(10), ["three", "four"]);
        logs.ingest(b"five\n").unwrap();
        assert_eq!(logs.tail(10), ["four", "five"]);

        logs.update_limits(1, 100).unwrap();
        assert_eq!(logs.tail(10), ["five"]);
    }

    #[test]
    fn timestamps_files_adds_notes_and_rotates_to_dot_one_log() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("server.log");
        let rotated = directory.path().join("server.1.log");
        let mut logs = PlainLogStore::file(&path, 10, 30).unwrap();

        logs.ingest_at(b"one\n", "01:02:03").unwrap();
        logs.note_at("restart", "01:02:04").unwrap();

        assert_eq!(fs::read_to_string(&rotated).unwrap(), "01:02:03 one\n");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "01:02:04 [netsustack] restart\n"
        );

        logs.ingest_at(b"current\n", "01:02:05").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "01:02:05 current\n");
        assert_eq!(
            fs::read_to_string(&rotated).unwrap(),
            "01:02:04 [netsustack] restart\n"
        );
    }

    #[test]
    fn reports_file_io_failures_as_typed_errors() {
        let directory = tempfile::tempdir().unwrap();

        let error = PlainLogStore::file(directory.path(), 10, 100).unwrap_err();

        assert_eq!(error.operation(), "open");
        assert_eq!(error.path(), directory.path());
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn bounds_ansi_transcript_by_bytes_and_lines_without_changing_live_chunks() {
        let mut transcript = AnsiTranscript::new(12, 2);

        let first = transcript.push(b"\x1b[31mred\x1b[0m\n").unwrap();
        let second = transcript.push(b"green\n").unwrap();
        let third = transcript.push(b"blue\n").unwrap();

        assert_eq!(first.data, b"\x1b[31mred\x1b[0m\n");
        assert_eq!((first.sequence, second.sequence, third.sequence), (1, 2, 3));
        assert!(transcript.byte_len() <= 12);
        assert!(transcript.line_count() <= 2);
        assert_eq!(transcript.replay().chunks.last().unwrap().data, b"blue\n");
    }

    #[test]
    fn oversized_ansi_chunk_is_dropped_whole_and_creates_a_replay_gap() {
        let mut transcript = AnsiTranscript::new(3, 10);

        let color = transcript.push(b"\x1b[31m").unwrap();
        assert_eq!(color.sequence, 1);
        assert!(transcript.replay().chunks.is_empty());
        assert_eq!(transcript.replay().first_sequence, None);
        assert_eq!(transcript.replay().next_sequence, 2);

        let text = transcript.push(b"ok").unwrap();
        let replay = transcript.replay();
        assert_eq!(text.sequence, 2);
        assert_eq!(replay.first_sequence, Some(2));
        assert_eq!(replay.chunks, [text]);

        let gap = transcript.replay_from(1).unwrap_err();
        assert_eq!(gap.available_from, 2);
        assert_eq!(gap.next_sequence, 3);
    }

    #[test]
    fn oversized_owned_ansi_chunk_is_live_only_without_reallocating_its_buffer() {
        let mut transcript = AnsiTranscript::new(3, 10);
        let data = vec![b'x'; 4_096];
        let allocation = data.as_ptr();

        let live = transcript.push_owned(data).unwrap();

        assert_eq!(live.data.as_ptr(), allocation);
        assert!(transcript.replay().chunks.is_empty());
        assert_eq!(transcript.replay().next_sequence, 2);
    }

    #[test]
    fn replay_is_sequenced_and_reports_unavailable_gaps() {
        let mut transcript = AnsiTranscript::new(8, 10);
        let first = transcript.push(b"one").unwrap();
        let second = transcript.push(b"two").unwrap();
        let third = transcript.push(b"three").unwrap();

        let replay = transcript.replay();
        assert_eq!(replay.first_sequence, Some(second.sequence));
        assert_eq!(replay.next_sequence, third.sequence + 1);
        assert_eq!(
            replay
                .chunks
                .iter()
                .map(|chunk| chunk.sequence)
                .collect::<Vec<_>>(),
            [second.sequence, third.sequence]
        );
        assert_eq!(
            transcript.replay_from(second.sequence).unwrap().chunks,
            replay.chunks
        );

        let gap = transcript.replay_from(first.sequence).unwrap_err();
        assert_eq!(gap.requested_sequence, first.sequence);
        assert_eq!(gap.available_from, second.sequence);
        assert_eq!(gap.next_sequence, third.sequence + 1);
    }

    #[test]
    fn empty_terminal_chunks_do_not_consume_sequences_and_clear_keeps_ordering() {
        let mut transcript = AnsiTranscript::new(100, 10);

        assert!(transcript.push(b"").is_none());
        assert_eq!(transcript.push(b"one").unwrap().sequence, 1);
        transcript.clear();
        assert_eq!(transcript.push(b"two").unwrap().sequence, 2);
        assert_eq!(transcript.replay().first_sequence, Some(2));
    }
}
