use crate::card::Card;
use crate::method;
use crate::method::Method as _;

// a call through a value needs the trait in scope, and names it nowhere
pub fn fee(card: &Card) -> u32 {
    card.fee() + method::flat()
}
