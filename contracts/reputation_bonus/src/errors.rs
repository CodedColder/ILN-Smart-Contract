use soroban_sdk::contracterror;

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum ContractError {
    ArithmeticError = 1,
    InvoiceNotFound = 2,
    IllegalState = 3,
    ConfigErrorUnauthorized = 4,
    ConfigErrorInvalidBonusBps = 5,
    ConfigErrorInvalidMinDiscountRate = 6,
    RateErrorArithmeticUnderflow = 7,
    RateErrorArithmeticOverflow = 8,
    ConfigErrorInvalidHighRepThreshold = 9,
}
