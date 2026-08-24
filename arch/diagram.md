# How Openstake Works

A plain-language walkthrough of the settlement engine. No code knowledge
needed. The technical version lives in `arch/USER_FLOWS.md`.

---

## The idea in one paragraph

Someone poses a question with two possible answers — *will Man Utd beat
Fulham?* People put money on the answer they believe. All that money goes
into one pot. When the real-world result comes in, everyone who backed the
right answer splits the pot between them, in proportion to how much they
put in and how early they got in. Nobody is betting against the house.
There is no house. Everyone is betting against each other.

---

## The cast

```mermaid
flowchart LR
    creator["The Creator\nposes the question,\npays a small fee to start"]
    bettor["The Bettors\npick a side,\nput money in"]
    authority["The Referee\nannounces the result\nwhen the match ends"]

    market["The Market\nthe question, the odds,\nand who's winning"]
    pot["The Pot\nwhere all the money sits"]
    slip["Your Slip\nproof of what you backed\nand how much"]

    creator --> market
    bettor --> market
    authority --> market
    market --- pot
    market --- slip

    classDef person fill:#f4f6f4,stroke:#9aa5ab,color:#3c4a52,stroke-width:1.5px
    classDef thing fill:#eef4f7,stroke:#7fa5b5,color:#1f4a58,stroke-width:1.5px
    classDef money fill:#f9f0e4,stroke:#d9a86b,color:#7a4413,stroke-width:1.5px
    class creator,bettor,authority person
    class market,slip thing
    class pot money
```

Three people, three things. **The Market** holds the question and the
current odds. **The Pot** holds the actual money. **Your Slip** is your
personal record — what you backed, how much of the pot you're entitled to
if you're right.

---

## The full journey

One market, beginning to end.

```mermaid
%%{init: {'theme':'base', 'themeVariables': {
  'actorBkg': '#eef4f7',
  'actorBorder': '#7fa5b5',
  'actorTextColor': '#1f4a58',
  'signalColor': '#8c9aa1',
  'signalTextColor': '#3c4a52',
  'noteBkgColor': '#f9f0e4',
  'noteTextColor': '#7a4413',
  'noteBorderColor': '#d9a86b',
  'labelBoxBkgColor': '#eef4f7',
  'labelBoxBorderColor': '#7fa5b5'
}}}%%
sequenceDiagram
    actor Joe as Joe
    actor Alice as Alice
    actor Ben as Ben
    actor Ref as Referee
    participant M as The Market
    participant P as The Pot

    Joe->>M: "Will Man Utd beat Fulham?"
    Note over M: opens at 50/50 — no one knows yet
    Joe->>P: pays the starting fee
    Note over Joe: Joe now holds a slip too —<br/>he's a bettor like anyone else

    Alice->>M: backs Man Utd
    Alice->>P: puts money in
    Note over M: odds shift toward Man Utd

    Ben->>M: backs Fulham
    Ben->>P: puts money in
    Note over M: odds shift back toward Fulham

    Note over Ref: the match is played
    Ref->>M: "Man Utd won"
    Note over M,P: no money moves yet —<br/>this only decides who was right

    Alice->>M: claims
    P->>Alice: her share of the pot
    Note over Alice: her slip is torn up

    Ben->>M: claims
    P->>Ben: nothing — he backed the loser
    Note over Ben: his slip is torn up too
```

**The thing to notice:** money goes in twice (Joe's fee, then each bet) and
comes out once (claiming). The referee's announcement moves no money at
all — it only settles the question of who was right. Losers still claim;
they just get nothing back except the small deposit they paid to hold a
slip in the first place.

---

## Joe creates a market

```mermaid
flowchart TB
    a["Joe writes the question\nand picks how many answers"] --> b["He pays a small starting fee"]
    b --> c["The fee is split evenly\nacross both answers"]
    c --> d["The market opens at 50/50"]
    d --> e["Joe walks away holding\na normal slip"]

    classDef step fill:#eef4f7,stroke:#7fa5b5,color:#1f4a58,stroke-width:1.5px
    classDef money fill:#f9f0e4,stroke:#d9a86b,color:#7a4413,stroke-width:1.5px
    class a,d,e step
    class b,c money
```

**Why the fee is split evenly:** if it all went on one side, the market
would open already leaning that way — and the first person to bet could
take advantage of that. Splitting it down the middle means the market
starts with genuinely no opinion.

**Joe gets no special treatment afterward.** He holds a slip on both
answers, exactly like anyone who'd bet that same amount. He can't cancel
the market, can't withdraw his fee, and doesn't decide the result.

---

## Someone places a bet

```mermaid
flowchart TB
    a["Alice picks a side\nand an amount"] --> b{"Has she bet on this\nmarket before?"}
    b -- "no" --> c["A new slip is made for her"]
    b -- "yes" --> d["Her existing slip is topped up"]
    c --> e["Her money goes into the pot"]
    d --> e
    e --> f["The odds shift"]

    classDef step fill:#eef4f7,stroke:#7fa5b5,color:#1f4a58,stroke-width:1.5px
    classDef money fill:#f9f0e4,stroke:#d9a86b,color:#7a4413,stroke-width:1.5px
    classDef choice fill:#f4f6f4,stroke:#9aa5ab,color:#3c4a52,stroke-width:1.5px
    class a,f step
    class b choice
    class c,d step
    class e money
```

**Once money is in, it stays in.** There's no way to change your mind and
pull out. The only way to get money back is to be right when the result
comes in.

---

## How the odds move

Nobody sets the odds. They come out of how much money is on each side.

```mermaid
flowchart LR
    a["50 / 50\nnobody has bet yet"] --> b["70 / 30\nheavy money on Man Utd"]
    b --> c["62 / 38\nsomeone backs Fulham"]

    classDef odds fill:#eef4f7,stroke:#7fa5b5,color:#1f4a58,stroke-width:1.5px
    class a,b,c odds
```

Two things follow from this, and both matter:

**Betting on the popular side gets you less.** If most money is already on
Man Utd, you're splitting the pot with more people, so your share is
smaller. Backing the unpopular side pays more if you're right — that's the
whole reason odds exist.

**Big bets move the price against themselves.** Put in a lot at once and
the odds shift while you're buying, so the last part of your bet gets a
worse rate than the first part. That's normal for any market — it's the
same reason selling a large amount of anything moves the price.

**Early on, this effect is strongest**, because there's very little money
in the pot yet and each new bet is a big fraction of it. The engine softens
this by treating the pot as if it's a bit larger than it really is when
working out odds — a cushion, so the first few bettors don't get hit as
hard by their own size. That cushion is only used for working out odds. It
is never real money, and it never affects what anyone gets paid.

---

## The result comes in

```mermaid
flowchart TB
    a["The match ends"] --> b["The referee announces\nthe winning side"]
    b --> c["The market is marked settled"]
    c --> d["Payouts are now unlocked"]

    classDef step fill:#eef4f7,stroke:#7fa5b5,color:#1f4a58,stroke-width:1.5px
    class a,b,c,d step
```

**Only one specific person can do this**, decided when the market was
created. Not the creator, not a bettor, not anyone who happens to be
watching. And it can only happen once — a settled market can't be settled
again with a different answer.

**No money moves at this step.** It only records who was right, which
unlocks the claiming step.

---

## Getting paid

```mermaid
flowchart TB
    a["Alice asks to claim"] --> b{"Did she back\nthe winning side?"}
    b -- "yes" --> c["She gets her share of the pot,\nsized by how much of the\nwinning side she holds"]
    b -- "no" --> d["She gets nothing from the pot"]
    c --> e["Her slip is torn up,\nher small deposit comes back"]
    d --> e

    classDef step fill:#eef4f7,stroke:#7fa5b5,color:#1f4a58,stroke-width:1.5px
    classDef money fill:#f9f0e4,stroke:#d9a86b,color:#7a4413,stroke-width:1.5px
    classDef choice fill:#f4f6f4,stroke:#9aa5ab,color:#3c4a52,stroke-width:1.5px
    class a step
    class b choice
    class c,e money
    class d step
```

**Your share** is straightforward: if you hold a tenth of all the winning
bets, you get a tenth of the pot. Everyone who was right divides the whole
pot between them, including the money the losers put in.

**You can only claim once.** The slip is marked and torn up in the same
step, so nobody can be paid twice for the same bet.

**Losers should still claim.** They get nothing from the pot, but they do
get back the small deposit that was charged to hold a slip in the first
place.

---

## The promise underneath all of this

**The pot never pays out more than went into it.** Not "shouldn't" —
can't. There's no formula that could produce a payout larger than the
money available, because payouts are always shares of a fixed pot, never
amounts calculated independently.

Before paying anyone, the engine checks that the money actually sitting in
the pot matches what its records say should be there. If those two numbers
ever disagree — by even a small amount — it refuses to pay out at all
rather than guess which one is right.

**Everything is checked before it's trusted.** Every time someone claims,
bets, or settles, the engine verifies that each thing handed to it really
is what it claims to be, really belongs to this market, and really belongs
to the person asking. Something in the wrong slot gets rejected outright,
not quietly misread.

---

## What isn't built yet

- **You can't sell your slip to someone else.** Once you're in, you're in
  until the result comes in. Trading slips between people is a later phase.
- **The referee is a fixed, trusted person**, not an automatic feed of
  real-world results. Replacing that with something automatic is a
  separate piece of work.
- **Markets have exactly two answers.** More than two is supported by the
  design but not switched on.

---

*Reflects the engine as currently built. The technical version, with the
actual account names and instruction details, is in `arch/USER_FLOWS.md`.*
