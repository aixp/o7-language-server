MODULE Lib;
  (* A list of numbers. *)
  CONST Max* = 10;
  TYPE
    Base* = RECORD id*: INTEGER; hidden: INTEGER END;
    Node* = POINTER TO NodeDesc;
    NodeDesc* = RECORD (Base) next*: Node; value*: INTEGER END;
  VAR count*: INTEGER;

  PROCEDURE Add*(VAR list: Node; value: INTEGER): INTEGER;
    VAR n: Node;
  BEGIN
    n := list; n.value := value; n.next := list; n.hidden := 0;
    list := n; INC(count);
    RETURN count
  END Add;

END Lib.
