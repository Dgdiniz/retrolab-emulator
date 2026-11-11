// YM2612 FM Algorithms - Operator Connection Patterns
//
// The YM2612 has 8 algorithms that define how the 4 operators are connected.
// Each algorithm creates different timbres through different modulation paths.
//
// Operators are numbered 1-4:
// - Operator 1 (OP1): Often used as modulator
// - Operator 2 (OP2): Often used as modulator
// - Operator 3 (OP3): Often used as modulator or carrier
// - Operator 4 (OP4): Always contributes to final output
//
// Algorithm numbering: 0-7 (as in YM2612 registers)

/// FM Algorithm identifier (0-7)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    /// Algorithm 0: OP1 -> OP2 -> OP3 -> OP4 -> Out
    /// All operators in series (maximum modulation depth)
    Algorithm0 = 0,

    /// Algorithm 1: (OP1 + OP2) -> OP3 -> OP4 -> Out
    /// OP1 and OP2 parallel, then series
    Algorithm1 = 1,

    /// Algorithm 2: (OP1 + (OP2 -> OP3)) -> OP4 -> Out
    /// OP1 and the OP2 -> OP3 pair both modulate OP4
    Algorithm2 = 2,

    /// Algorithm 3: ((OP1 -> OP2) + OP3) -> OP4 -> Out
    /// The OP1 -> OP2 pair and OP3 both modulate OP4
    Algorithm3 = 3,

    /// Algorithm 4: (OP1 -> OP2) + (OP3 -> OP4) -> Out
    /// Two parallel FM pairs
    Algorithm4 = 4,

    /// Algorithm 5: OP1 -> (OP2 + OP3 + OP4) -> Out
    /// OP1 modulates all other operators
    Algorithm5 = 5,

    /// Algorithm 6: (OP1 -> OP2) + OP3 + OP4 -> Out
    /// One FM pair + two carriers
    Algorithm6 = 6,

    /// Algorithm 7: OP1 + OP2 + OP3 + OP4 -> Out
    /// All operators are carriers (additive synthesis)
    Algorithm7 = 7,
}

impl Algorithm {
    /// Create algorithm from u8 (0-7)
    pub fn from_u8(value: u8) -> Self {
        match value & 0x07 {
            0 => Algorithm::Algorithm0,
            1 => Algorithm::Algorithm1,
            2 => Algorithm::Algorithm2,
            3 => Algorithm::Algorithm3,
            4 => Algorithm::Algorithm4,
            5 => Algorithm::Algorithm5,
            6 => Algorithm::Algorithm6,
            7 => Algorithm::Algorithm7,
            _ => unreachable!(),
        }
    }

    /// Get algorithm as u8
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    /// Get human-readable description
    pub fn description(self) -> &'static str {
        match self {
            Algorithm::Algorithm0 => "All series: OP1->OP2->OP3->OP4",
            Algorithm::Algorithm1 => "Parallel start: (OP1+OP2)->OP3->OP4",
            Algorithm::Algorithm2 => "One and a pair: (OP1+(OP2->OP3))->OP4",
            Algorithm::Algorithm3 => "A pair and one: ((OP1->OP2)+OP3)->OP4",
            Algorithm::Algorithm4 => "Two FM pairs: (OP1->OP2)+(OP3->OP4)",
            Algorithm::Algorithm5 => "Full mod: OP1->(OP2+OP3+OP4)",
            Algorithm::Algorithm6 => "One pair: (OP1->OP2)+OP3+OP4",
            Algorithm::Algorithm7 => "All parallel: OP1+OP2+OP3+OP4",
        }
    }
}

/// Execute FM algorithm on 4 operator outputs: the sum of the algorithm's
/// carriers, the operators already clocked with their modulation (the
/// channel routes the modulation itself and does not call this).
///
/// # Arguments
/// * `algorithm` - The algorithm to use (0-7)
/// * `op_outputs` - Array of 4 operator outputs [OP1, OP2, OP3, OP4]
/// * `feedback` - Feedback value (0-7) for OP1
/// * `last_op1` - Last OP1 output (for feedback)
///
/// # Returns
/// Tuple of (channel_output, new_last_op1)
pub fn execute_algorithm(
    algorithm: Algorithm,
    op_outputs: &mut [i16; 4],
    feedback: u8,
    last_op1: i16,
) -> (i16, i16) {
    // Calculate feedback modulation for OP1
    // Feedback: 0 = no feedback, 7 = maximum feedback
    let _fb_mod = if feedback == 0 {
        0
    } else {
        // Feedback is self-modulation of OP1
        // Shift right by (8 - feedback) to scale
        last_op1 >> (8 - feedback)
    };

    // Note: In a real implementation, we'd clock the operators here with modulation
    // For this implementation, we assume operators are already clocked and we're
    // just routing their outputs according to the algorithm
    // Feedback is handled at the channel level, not here

    match algorithm {
        Algorithm::Algorithm0 => {
            // OP1 -> OP2 -> OP3 -> OP4 -> Out
            // Full cascade modulation
            // In practice: OP1 modulates OP2, OP2 modulates OP3, OP3 modulates OP4
            // Here we just return OP4 as final output (operators are pre-modulated)
            let output = op_outputs[3]; // OP4
            (output, op_outputs[0]) // Save OP1 for feedback
        }

        Algorithm::Algorithm1 => {
            // (OP1 + OP2) -> OP3 -> OP4 -> Out
            // OP1 and OP2 both modulate OP3, OP3 modulates OP4
            let output = op_outputs[3]; // OP4
            (output, op_outputs[0])
        }

        Algorithm::Algorithm2 => {
            // (OP1 + (OP2 -> OP3)) -> OP4 -> Out
            // OP1 and the OP2 -> OP3 pair both modulate OP4
            let output = op_outputs[3]; // OP4
            (output, op_outputs[0])
        }

        Algorithm::Algorithm3 => {
            // ((OP1 -> OP2) + OP3) -> OP4 -> Out
            // The OP1 -> OP2 pair and OP3 both modulate OP4
            let output = op_outputs[3]; // OP4
            (output, op_outputs[0])
        }

        Algorithm::Algorithm4 => {
            // (OP1 -> OP2) + (OP3 -> OP4) -> Out
            // Two parallel FM pairs, their carriers summed
            let output = op_outputs[1].saturating_add(op_outputs[3]);
            (output, op_outputs[0])
        }

        Algorithm::Algorithm5 => {
            // OP1 -> (OP2 + OP3 + OP4) -> Out
            // OP1 modulates all others
            let output = op_outputs[1]
                .saturating_add(op_outputs[2])
                .saturating_add(op_outputs[3]);
            (output, op_outputs[0])
        }

        Algorithm::Algorithm6 => {
            // (OP1 -> OP2) + OP3 + OP4 -> Out
            // One FM pair plus two carriers
            let output = op_outputs[1]
                .saturating_add(op_outputs[2])
                .saturating_add(op_outputs[3]);
            (output, op_outputs[0])
        }

        Algorithm::Algorithm7 => {
            // OP1 + OP2 + OP3 + OP4 -> Out
            // All operators are carriers (additive)
            let output = op_outputs[0]
                .saturating_add(op_outputs[1])
                .saturating_add(op_outputs[2])
                .saturating_add(op_outputs[3]);
            (output, op_outputs[0])
        }
    }
}

/// Get modulation routing for an algorithm
///
/// Returns tuples of (modulator_index, carrier_index) for each operator connection.
/// Indices are 0-3 for operators 1-4.
pub fn get_modulation_routing(algorithm: Algorithm) -> Vec<(usize, usize)> {
    match algorithm {
        Algorithm::Algorithm0 => {
            // OP1 -> OP2 -> OP3 -> OP4
            vec![(0, 1), (1, 2), (2, 3)]
        }
        Algorithm::Algorithm1 => {
            // (OP1 + OP2) -> OP3 -> OP4
            vec![(0, 2), (1, 2), (2, 3)]
        }
        Algorithm::Algorithm2 => {
            // (OP1 + (OP2 -> OP3)) -> OP4
            vec![(1, 2), (0, 3), (2, 3)]
        }
        Algorithm::Algorithm3 => {
            // ((OP1 -> OP2) + OP3) -> OP4
            vec![(0, 1), (1, 3), (2, 3)]
        }
        Algorithm::Algorithm4 => {
            // (OP1 -> OP2) + (OP3 -> OP4)
            vec![(0, 1), (2, 3)]
        }
        Algorithm::Algorithm5 => {
            // OP1 -> (OP2 + OP3 + OP4)
            vec![(0, 1), (0, 2), (0, 3)]
        }
        Algorithm::Algorithm6 => {
            // (OP1 -> OP2) + OP3 + OP4
            vec![(0, 1)]
        }
        Algorithm::Algorithm7 => {
            // All parallel (no modulation)
            vec![]
        }
    }
}

/// Get which operators contribute to final output (the algorithm's carriers)
pub fn get_output_operators(algorithm: Algorithm) -> Vec<usize> {
    match algorithm {
        Algorithm::Algorithm0 => vec![3],          // Only OP4
        Algorithm::Algorithm1 => vec![3],          // Only OP4
        Algorithm::Algorithm2 => vec![3],          // Only OP4
        Algorithm::Algorithm3 => vec![3],          // Only OP4
        Algorithm::Algorithm4 => vec![1, 3],       // OP2 + OP4
        Algorithm::Algorithm5 => vec![1, 2, 3],    // OP2 + OP3 + OP4
        Algorithm::Algorithm6 => vec![1, 2, 3],    // OP2 + OP3 + OP4
        Algorithm::Algorithm7 => vec![0, 1, 2, 3], // All operators
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_algorithm_from_u8() {
        assert_eq!(Algorithm::from_u8(0), Algorithm::Algorithm0);
        assert_eq!(Algorithm::from_u8(7), Algorithm::Algorithm7);
        assert_eq!(Algorithm::from_u8(8), Algorithm::Algorithm0); // Wraps
        assert_eq!(Algorithm::from_u8(15), Algorithm::Algorithm7); // Wraps
    }

    #[test]
    fn test_algorithm_as_u8() {
        for i in 0..8 {
            let algo = Algorithm::from_u8(i);
            assert_eq!(algo.as_u8(), i);
        }
    }

    #[test]
    fn test_algorithm_descriptions() {
        for i in 0..8 {
            let algo = Algorithm::from_u8(i);
            let desc = algo.description();
            assert!(!desc.is_empty());
            println!("Algorithm {}: {}", i, desc);
        }
    }

    #[test]
    fn test_execute_algorithm0() {
        // Algorithm 0: All series
        let mut ops = [100i16, 200, 300, 400];
        let (output, fb) = execute_algorithm(Algorithm::Algorithm0, &mut ops, 0, 0);

        // Should output OP4
        assert_eq!(output, 400);
        assert_eq!(fb, 100); // Saves OP1 for feedback
    }

    #[test]
    fn test_execute_algorithm7() {
        // Algorithm 7: All parallel (additive)
        let mut ops = [100i16, 200, 300, 400];
        let (output, fb) = execute_algorithm(Algorithm::Algorithm7, &mut ops, 0, 0);

        // Should sum all operators
        assert_eq!(output, 1000);
        assert_eq!(fb, 100);
    }

    #[test]
    fn test_execute_algorithms_2_to_4() {
        let mut ops = [100i16, 200, 300, 400];
        // Algorithms 2 and 3 end in OP4 alone
        let (output, fb) = execute_algorithm(Algorithm::Algorithm2, &mut ops, 0, 0);
        assert_eq!(output, 400);
        assert_eq!(fb, 100);
        let (output, _) = execute_algorithm(Algorithm::Algorithm3, &mut ops, 0, 0);
        assert_eq!(output, 400);
        // Algorithm 4: the two pairs' carriers, OP2 + OP4
        let (output, _) = execute_algorithm(Algorithm::Algorithm4, &mut ops, 0, 0);
        assert_eq!(output, 600);
    }

    #[test]
    fn test_feedback() {
        let mut ops = [1000i16, 0, 0, 0];
        let last_op1 = 2000i16;

        // No feedback
        let (_, fb0) = execute_algorithm(Algorithm::Algorithm0, &mut ops, 0, last_op1);

        // Max feedback (7)
        let (_, fb7) = execute_algorithm(Algorithm::Algorithm0, &mut ops, 7, last_op1);

        // Feedback should save OP1
        assert_eq!(fb0, 1000);
        assert_eq!(fb7, 1000);
    }

    #[test]
    fn test_modulation_routing() {
        // Algorithm 0: linear chain
        let routing = get_modulation_routing(Algorithm::Algorithm0);
        assert_eq!(routing.len(), 3);
        assert!(routing.contains(&(0, 1))); // OP1 -> OP2
        assert!(routing.contains(&(1, 2))); // OP2 -> OP3
        assert!(routing.contains(&(2, 3))); // OP3 -> OP4

        // Algorithm 7: no modulation
        let routing = get_modulation_routing(Algorithm::Algorithm7);
        assert_eq!(routing.len(), 0);
    }

    #[test]
    fn test_output_operators() {
        // Algorithm 0: only OP4 outputs
        let outputs = get_output_operators(Algorithm::Algorithm0);
        assert_eq!(outputs, vec![3]);

        // Algorithm 7: all output
        let outputs = get_output_operators(Algorithm::Algorithm7);
        assert_eq!(outputs, vec![0, 1, 2, 3]);

        // Algorithms 2 and 3: OP4 alone; algorithm 4: OP2 and OP4
        assert_eq!(get_output_operators(Algorithm::Algorithm2), vec![3]);
        assert_eq!(get_output_operators(Algorithm::Algorithm3), vec![3]);
        assert_eq!(get_output_operators(Algorithm::Algorithm4), vec![1, 3]);
    }

    #[test]
    fn test_routing_of_algorithms_2_to_4() {
        // As the channel runs them (channel.rs): OP2 -> OP3 and OP1 into OP4
        let routing = get_modulation_routing(Algorithm::Algorithm2);
        assert_eq!(routing, vec![(1, 2), (0, 3), (2, 3)]);
        // OP1 -> OP2, and OP2 with OP3 into OP4
        let routing = get_modulation_routing(Algorithm::Algorithm3);
        assert_eq!(routing, vec![(0, 1), (1, 3), (2, 3)]);
        // Two pairs: OP1 -> OP2, OP3 -> OP4
        let routing = get_modulation_routing(Algorithm::Algorithm4);
        assert_eq!(routing, vec![(0, 1), (2, 3)]);
    }

    #[test]
    fn test_all_algorithms_routing() {
        // Ensure all algorithms have valid routing
        for i in 0..8 {
            let algo = Algorithm::from_u8(i);
            let routing = get_modulation_routing(algo);
            let outputs = get_output_operators(algo);

            println!(
                "Algorithm {}: {} routes, {} outputs",
                i,
                routing.len(),
                outputs.len()
            );

            // All algorithms should have at least one output
            assert!(!outputs.is_empty(), "Algorithm {} has no outputs", i);

            // Output operators should be in range 0-3
            for &op in &outputs {
                assert!(op < 4, "Invalid output operator {}", op);
            }

            // Modulation routes should be valid
            for &(mod_op, car_op) in &routing {
                assert!(mod_op < 4, "Invalid modulator {}", mod_op);
                assert!(car_op < 4, "Invalid carrier {}", car_op);
                assert_ne!(mod_op, car_op, "Self-modulation not via feedback");
            }
        }
    }

    #[test]
    fn test_saturating_add() {
        // Test that summing doesn't overflow
        let mut ops = [i16::MAX / 2, i16::MAX / 2, i16::MAX / 2, i16::MAX / 2];
        let (output, _) = execute_algorithm(Algorithm::Algorithm7, &mut ops, 0, 0);

        // Should saturate instead of overflow
        assert_eq!(output, i16::MAX);
    }
}
