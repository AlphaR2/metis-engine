use wincode::{SchemaRead, SchemaWrite};

#[derive(SchemaWrite, SchemaRead, Default)]
pub struct Football {
    pub match_id: [u8; 4],
    pub score: u8, // this is a bitpack - higher 4 bits is home, lower 4 bits is away
    pub penalties: u8, // this is a bitpack - higher 4 bits is home, lower 4 bits is away
    pub status: FootballMatchPhase, // 4 bytes
}

#[allow(unused)]
impl Football {
    pub fn length(&self) -> usize {
        let lenn = wincode::serialized_size(&Football::default()).expect("didn't deser");

        lenn as usize
    }

    /// 0x0F is 0000_1111
    ///
    /// and we want to find all numbers in the lower half `_1111`
    ///
    /// whenever 1 & 1 is true, it returns it else it returns a 0
    /// ### example:
    ///```text
    /// 0011_1001 - original value
    ///&
    /// 0000_1111
    /// ----------
    /// 0000_1001
    /// ---------
    /// ```
    /// this has helped us extract the `_1001` at the lower half
    pub fn get_score(&self) -> (u8, u8) {
        let home_score = self.score >> 4;
        let away_score = self.score & 0x0F;

        (home_score, away_score)
    }

    /// ### see `get_score()` docs
    pub fn get_penalties(&self) -> (u8, u8) {
        let home_pen = self.penalties >> 4;
        let away_pen = self.penalties & 0x0F;

        (home_pen, away_pen)
    }
}

#[derive(SchemaWrite, SchemaRead, Default)]
pub enum FootballMatchPhase {
    #[default]
    Scheduled = 0,
    FirstHalf = 1,
    HalfTime = 2,
    SecondHalf = 3,
    ExtraTimeFirstHalf = 4,
    ExtraTimeBreak = 5,
    ExtraTimeSecondHalf = 6,
    Penalties = 7,
    Finished = 8,
}
