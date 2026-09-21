//! Wallet data and transfer rules, independent of session lifecycle and publication.

/// Recorded setup used to construct authority, replica, and replay state.
#[derive(Debug, Clone, PartialEq, Eq, synctick::Wire)]
pub struct Initialization {
    /// Recorded seed included in the state hash. This game draws no random numbers.
    pub seed: u64,
    /// Initial balances indexed by wallet ID. At least one wallet is required.
    pub balances: Vec<u64>,
}

/// One atomic transfer from the submitting participant's wallet.
///
/// The amounts are summed with checked arithmetic. An empty list transfers zero.
/// Invalid transfers change no balances and increment [`State::rejected`] once.
#[derive(Debug, Clone, PartialEq, Eq, synctick::Wire)]
pub struct Transfer {
    /// Destination wallet index, independent of the sender's participant ID.
    pub recipient: u64,
    /// Amounts to add together before transferring; the entire command is atomic.
    pub amounts: Vec<u64>,
}

/// Complete deterministic state, also used for immutable presentation snapshots.
#[derive(Debug, Clone, Default, PartialEq, Eq, synctick::StableHash)]
pub struct State {
    /// Last completed framework tick; zero before the first tick.
    pub tick: u64,
    /// Seed from the recorded initialization.
    pub seed: u64,
    /// Current balances indexed by wallet ID.
    pub balances: Vec<u64>,
    /// Number of rejected commands, including unauthorized participants.
    pub rejected: u64,
}

/// Internal rule failures. They are gameplay rejections, not session errors.
#[derive(Debug, PartialEq, Eq)]
pub enum Rejection {
    UnknownSender,
    UnknownRecipient,
    AmountOverflow,
    InsufficientFunds,
    RecipientOverflow,
}

impl State {
    /// Hash all deterministic state with the framework's stable representation.
    ///
    /// Authority, replicas, and replay use this same hash. Presentation snapshots
    /// can be compared with replay outcomes without reconstructing a simulation.
    #[must_use]
    pub fn state_hash(&self) -> u64 {
        synctick::stable_hash(self)
    }

    /// Validate the whole transfer before mutating either balance.
    pub(crate) fn transfer(&mut self, sender: u64, command: &Transfer) -> Result<(), Rejection> {
        let sender = usize::try_from(sender).map_err(|_| Rejection::UnknownSender)?;
        let recipient =
            usize::try_from(command.recipient).map_err(|_| Rejection::UnknownRecipient)?;
        let amount = command.amounts.iter().try_fold(0u64, |sum, amount| {
            sum.checked_add(*amount).ok_or(Rejection::AmountOverflow)
        })?;
        let balance = self.balances.get(sender).ok_or(Rejection::UnknownSender)?;
        let remaining = balance
            .checked_sub(amount)
            .ok_or(Rejection::InsufficientFunds)?;

        // Self-transfers still require sufficient funds but change no balances.
        if sender == recipient {
            return Ok(());
        }
        let destination = self
            .balances
            .get(recipient)
            .ok_or(Rejection::UnknownRecipient)?;
        let received = destination
            .checked_add(amount)
            .ok_or(Rejection::RecipientOverflow)?;
        self.balances[sender] = remaining;
        self.balances[recipient] = received;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_authoritative_field_participates_in_hash() {
        let state = State {
            tick: 1,
            seed: 42,
            balances: vec![10, 20],
            rejected: 0,
        };
        for edit in [
            |s: &mut State| s.tick += 1,
            |s: &mut State| s.seed += 1,
            |s: &mut State| s.rejected += 1,
            |s: &mut State| s.balances[0] += 1,
            |s: &mut State| s.balances[1] += 1,
            |s: &mut State| s.balances.push(0),
        ] {
            let mut changed = state.clone();
            edit(&mut changed);
            assert_ne!(state.state_hash(), changed.state_hash());
        }
    }

    #[test]
    fn rejected_transfers_never_partially_update_balances() {
        let cases = [
            (9, 1, vec![1], Rejection::UnknownSender),
            (0, 9, vec![1], Rejection::UnknownRecipient),
            (0, 1, vec![u64::MAX, 1], Rejection::AmountOverflow),
            (0, 1, vec![11], Rejection::InsufficientFunds),
            (0, 1, vec![1], Rejection::RecipientOverflow),
        ];
        for (sender, recipient, amounts, expected) in cases {
            let mut state = State {
                balances: vec![10, u64::MAX],
                ..State::default()
            };
            let before = state.clone();
            assert_eq!(
                state.transfer(sender, &Transfer { recipient, amounts }),
                Err(expected)
            );
            assert_eq!(state, before);
        }
    }

    #[test]
    fn transfers_are_atomic_and_self_transfers_still_check_funds() {
        let mut state = State {
            balances: vec![10, 20],
            ..State::default()
        };
        state
            .transfer(
                0,
                &Transfer {
                    recipient: 1,
                    amounts: vec![3, 7],
                },
            )
            .unwrap();
        assert_eq!(state.balances, [0, 30]);
        state
            .transfer(
                1,
                &Transfer {
                    recipient: 1,
                    amounts: vec![30],
                },
            )
            .unwrap();
        state
            .transfer(
                0,
                &Transfer {
                    recipient: 1,
                    amounts: vec![],
                },
            )
            .unwrap();
        assert_eq!(state.balances, [0, 30]);
        assert_eq!(
            state.transfer(
                1,
                &Transfer {
                    recipient: 1,
                    amounts: vec![31]
                }
            ),
            Err(Rejection::InsufficientFunds)
        );
        assert_eq!(state.balances, [0, 30]);
    }
}
