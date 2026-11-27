//! `users.usersSlice` counts at the ends of the `i32` range (TGB-3090).

use tg_bots::tl_convert::bot_recommendations_from_tl;
use tg_tl::{enums as e, types as t};

fn slice(count: i32, users: usize) -> e::users::Users {
    e::users::Users::Slice(t::users::UsersSlice {
        count,
        users: (1..=users as i64)
            .map(|id| e::User::Empty(t::UserEmpty { id }))
            .collect(),
    })
}

#[test]
fn more_count_saturates_on_min_count() {
    let rec = bot_recommendations_from_tl(&slice(i32::MIN, 1));
    assert_eq!(rec.bot_ids.len(), 1);
    assert_eq!(rec.more_count, 0);
}

#[test]
fn more_count_normal_values_unchanged() {
    assert_eq!(bot_recommendations_from_tl(&slice(10, 3)).more_count, 7);
    assert_eq!(bot_recommendations_from_tl(&slice(2, 3)).more_count, 0);
    assert_eq!(
        bot_recommendations_from_tl(&slice(i32::MAX, 2)).more_count,
        i32::MAX - 2
    );
}
