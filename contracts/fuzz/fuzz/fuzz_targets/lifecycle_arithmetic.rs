#![no_main]

use arbitrary::Arbitrary;
use iln_fuzz::lifecycle_arithmetic::{checked_discount, checked_funding, checked_payment};
use libfuzzer_sys::fuzz_target;

#[derive(Arbitrary, Debug)]
struct LifecycleArithmeticInput {
    amount: i128,
    funded: i128,
    contribution: i128,
    paid: i128,
    payment: i128,
    fee_bps: u32,
    discount_bps: u32,
}

fuzz_target!(|input: LifecycleArithmeticInput| {
    if let Ok(result) = checked_funding(input.amount, input.funded, input.contribution) {
        assert!(result.total_funded <= input.amount);
        assert!(result.total_funded >= input.funded);
        assert_eq!(result.accepted, input.contribution.min(result.remaining + result.accepted));
        assert_eq!(result.total_funded + result.remaining, input.amount);
    }

    if let Ok(result) = checked_payment(input.amount, input.paid, input.payment, input.fee_bps) {
        assert!(result.total_paid <= input.amount);
        assert!(result.fee <= input.payment);
        assert_eq!(result.fee + result.payout, input.payment);
    }

    if let Ok(discounted) = checked_discount(input.amount, input.discount_bps) {
        assert!(discounted <= input.amount);
    }
});
